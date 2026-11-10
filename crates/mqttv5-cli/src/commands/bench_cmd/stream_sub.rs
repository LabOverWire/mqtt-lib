use super::{hol_topic_index, BenchCommand, HolRecorder, HOL_TOPIC_PREFIX, HOL_WILDCARD_FILTER};
use anyhow::{anyhow, bail, Context, Result};
use bytes::BytesMut;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::packet::subscribe::SubscribePacket;
use mqtt5::transport::packet_io::{encode_packet_to_buffer, read_packet_from_stream};
use mqtt5::transport::{QuicConfig, QuicTransport, TlsConfig};
use mqtt5::{ConnectOptions, Packet, QoS, Transport};
use quinn::{RecvStream, SendStream};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

const PROTOCOL_VERSION: u8 = 5;
const MAX_PACKET_SIZE: usize = 1024 * 1024;
const KEEP_ALIVE: Duration = Duration::from_secs(30);
const SUBSCRIBE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) struct StreamSubscriber {
    connection: Arc<quinn::Connection>,
    tasks: Vec<(String, JoinHandle<Result<()>>)>,
    subscription_sends: Vec<SendStream>,
}

impl StreamSubscriber {
    pub(super) fn connection(&self) -> Arc<quinn::Connection> {
        Arc::clone(&self.connection)
    }

    async fn check(&mut self) -> Result<()> {
        let Some(finished) = self.tasks.iter().position(|(_, task)| task.is_finished()) else {
            return Ok(());
        };
        let (label, task) = self.tasks.swap_remove(finished);
        match task.await {
            Ok(Ok(())) => Err(anyhow!("{label} ended before the run finished")),
            Ok(Err(e)) => Err(e.context(format!("{label} ended before the run finished"))),
            Err(e) => Err(anyhow!("{label} task failed: {e}")),
        }
    }

    pub(super) fn close(self) {
        for (_, task) in &self.tasks {
            task.abort();
        }
        drop(self.subscription_sends);
        self.connection.close(0u32.into(), b"bench done");
    }
}

pub(super) async fn check_all(
    subscribers: &mut [StreamSubscriber],
    recorder: &HolRecorder,
    wildcard: bool,
) -> Result<()> {
    if subscribers.is_empty() {
        return Ok(());
    }
    for subscriber in subscribers.iter_mut() {
        subscriber.check().await?;
    }
    let mut per_connection = vec![0usize; subscribers.len()];
    let mut empty_topics = Vec::new();
    for (topic_idx, samples) in recorder.topic_samples.iter().enumerate() {
        let samples = samples
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if samples.is_empty() {
            empty_topics.push(format!("{HOL_TOPIC_PREFIX}{topic_idx}"));
        }
        for sample in samples.iter() {
            if let Some(count) = per_connection.get_mut(usize::from(sample.conn_idx)) {
                *count += 1;
            }
        }
    }
    if !empty_topics.is_empty() {
        bail!(
            "stream subscribers received no messages in the measurement window for {}",
            empty_topics.join(", ")
        );
    }
    if wildcard {
        if let Some(conn) = per_connection.iter().position(|&count| count == 0) {
            bail!("subscriber connection {conn} received no messages in the measurement window");
        }
    }
    Ok(())
}

struct Subscription {
    filter: String,
    topic_idx: Option<usize>,
}

pub(super) async fn connect_all(
    cmd: &BenchCommand,
    base_id: &str,
    url: &str,
    sub_conns: usize,
    recorder: &Arc<HolRecorder>,
) -> Result<Vec<StreamSubscriber>> {
    let num_topics = recorder.topic_samples.len();
    let mut subscribers = Vec::with_capacity(sub_conns);
    for conn in 0..sub_conns {
        let subscriptions: Vec<Subscription> = if cmd.hol_wildcard {
            vec![Subscription {
                filter: HOL_WILDCARD_FILTER.to_string(),
                topic_idx: None,
            }]
        } else {
            (conn..num_topics)
                .step_by(sub_conns)
                .map(|topic_idx| Subscription {
                    filter: format!("{HOL_TOPIC_PREFIX}{topic_idx}"),
                    topic_idx: Some(topic_idx),
                })
                .collect()
        };
        let client_id = format!("{base_id}-sub-{conn}");
        let conn_idx = u16::try_from(conn).unwrap_or(u16::MAX);
        subscribers.push(
            connect_one(
                quic_config(cmd, url).await?,
                &client_id,
                conn_idx,
                subscriptions,
                recorder,
            )
            .await
            .with_context(|| format!("stream subscriber {client_id}"))?,
        );
    }
    eprintln!("subscribed on one QUIC stream per subscription over {sub_conns} connection(s)");
    Ok(subscribers)
}

pub(super) fn quic_url(url: &str) -> Result<(bool, &str)> {
    match url.split_once("://") {
        Some(("quic", rest)) => Ok((false, rest)),
        Some(("quics", rest)) => Ok((true, rest)),
        _ => bail!("--hol-subscription-streams needs a quic:// or quics:// URL, got {url}"),
    }
}

async fn quic_config(cmd: &BenchCommand, url: &str) -> Result<QuicConfig> {
    let (secure, rest) = quic_url(url)?;
    let authority = rest.split('/').next().unwrap_or(rest);
    let addr = tokio::net::lookup_host(authority)
        .await
        .with_context(|| format!("failed to resolve {authority} (host:port required)"))?
        .next()
        .with_context(|| format!("no address for {authority}"))?;
    let host = authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
        .trim_start_matches('[')
        .trim_end_matches(']');
    let server_name = if host.parse::<std::net::IpAddr>().is_ok() {
        "localhost"
    } else {
        host
    };
    let mut config = QuicConfig::new(addr, server_name)
        .with_verify_server_cert(secure && !cmd.insecure)
        .with_connect_timeout(Duration::from_secs(cmd.quic_connect_timeout));
    if secure {
        let mut tls = TlsConfig::new(addr, server_name);
        if let Some(path) = &cmd.ca_cert {
            let pem = std::fs::read(path)
                .with_context(|| format!("failed to read CA cert: {}", path.display()))?;
            tls.load_ca_cert_pem_bytes(&pem)?;
        }
        if let (Some(cert), Some(key)) = (&cmd.cert, &cmd.key) {
            let cert_pem = std::fs::read(cert)
                .with_context(|| format!("failed to read cert: {}", cert.display()))?;
            let key_pem = std::fs::read(key)
                .with_context(|| format!("failed to read key: {}", key.display()))?;
            tls.load_client_cert_pem_bytes(&cert_pem)?;
            tls.load_client_key_pem_bytes(&key_pem)?;
        }
        if let (Some(chain), Some(key)) = (tls.client_cert, tls.client_key) {
            config = config.with_client_cert(chain, key);
        }
        if let Some(roots) = tls.root_certs {
            config = config.with_root_certs(roots);
        }
    }
    Ok(config)
}

async fn write_packet(send: &mut SendStream, packet: &Packet) -> Result<()> {
    let mut buf = BytesMut::new();
    encode_packet_to_buffer(packet, &mut buf)?;
    send.write_all(&buf).await?;
    Ok(())
}

async fn read_packet(recv: &mut RecvStream, buffer: &mut BytesMut) -> Result<Packet> {
    Ok(read_packet_from_stream(recv, PROTOCOL_VERSION, buffer, MAX_PACKET_SIZE).await?)
}

async fn connect_one(
    config: QuicConfig,
    client_id: &str,
    conn_idx: u16,
    subscriptions: Vec<Subscription>,
    recorder: &Arc<HolRecorder>,
) -> Result<StreamSubscriber> {
    let mut transport = QuicTransport::new(config);
    transport.connect().await.context("QUIC connect failed")?;
    let split = transport.into_split()?;
    let (mut control_send, mut control_recv) = (split.send, split.recv);
    let connection = Arc::new(split.connection);

    let options = ConnectOptions::new(client_id)
        .with_clean_start(true)
        .with_keep_alive(KEEP_ALIVE);
    let connect = Packet::Connect(Box::new(ConnectPacket::new(options.protocol_options)));
    write_packet(&mut control_send, &connect).await?;
    let mut control_buffer = BytesMut::new();
    match read_packet(&mut control_recv, &mut control_buffer).await? {
        Packet::ConnAck(connack) if connack.reason_code.is_success() => {}
        Packet::ConnAck(connack) => bail!("CONNACK refused: {:?}", connack.reason_code),
        other => bail!("expected CONNACK, got {other:?}"),
    }

    let mut tasks = vec![
        (
            "server stream watch".to_string(),
            tokio::spawn(refuse_server_streams(Arc::clone(&connection))),
        ),
        (
            "control stream keep-alive".to_string(),
            tokio::spawn(ping_control(control_send)),
        ),
        (
            "control stream".to_string(),
            tokio::spawn(read_control(control_recv, control_buffer)),
        ),
    ];
    let mut subscription_sends = Vec::with_capacity(subscriptions.len());
    for (index, subscription) in subscriptions.into_iter().enumerate() {
        let packet_id = u16::try_from(index + 1).context("too many subscriptions")?;
        let (send, recv, buffer) = tokio::time::timeout(
            SUBSCRIBE_TIMEOUT,
            subscribe_on_new_stream(&connection, packet_id, &subscription.filter),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "subscribing to {} on its own stream took over {}s",
                subscription.filter,
                SUBSCRIBE_TIMEOUT.as_secs()
            )
        })??;
        let label = format!(
            "subscription stream {} for {}",
            u64::from(recv.id()),
            subscription.filter
        );
        subscription_sends.push(send);
        tasks.push((
            label,
            tokio::spawn(read_deliveries(
                recv,
                buffer,
                subscription,
                conn_idx,
                Arc::clone(recorder),
            )),
        ));
    }
    Ok(StreamSubscriber {
        connection,
        tasks,
        subscription_sends,
    })
}

async fn subscribe_on_new_stream(
    connection: &quinn::Connection,
    packet_id: u16,
    filter: &str,
) -> Result<(SendStream, RecvStream, BytesMut)> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .with_context(|| format!("failed to open a stream for {filter}"))?;
    let subscribe = SubscribePacket::new(packet_id).add_filter(filter, QoS::AtMostOnce);
    write_packet(&mut send, &Packet::Subscribe(subscribe)).await?;
    let mut buffer = BytesMut::new();
    let stream_id = u64::from(recv.id());
    match read_packet(&mut recv, &mut buffer)
        .await
        .with_context(|| format!("stream {stream_id} for {filter} ended before its SUBACK"))?
    {
        Packet::SubAck(suback) if suback.packet_id == packet_id => {
            match suback.reason_codes.as_slice() {
                [code] if code.granted_qos().is_some() => Ok((send, recv, buffer)),
                codes => bail!("SUBSCRIBE to {filter} on stream {stream_id} refused: {codes:?}"),
            }
        }
        other => bail!("expected the SUBACK for {filter} on stream {stream_id}, got {other:?}"),
    }
}

async fn refuse_server_streams(connection: Arc<quinn::Connection>) -> Result<()> {
    let stream_id = tokio::select! {
        bi = connection.accept_bi() => u64::from(bi?.1.id()),
        uni = connection.accept_uni() => u64::from(uni?.id()),
    };
    bail!(
        "broker opened stream {stream_id} for deliveries; the stream subscriber reads only the streams it opened for its subscriptions"
    )
}

async fn ping_control(mut send: SendStream) -> Result<()> {
    let mut interval = tokio::time::interval(KEEP_ALIVE / 2);
    interval.tick().await;
    loop {
        interval.tick().await;
        write_packet(&mut send, &Packet::PingReq).await?;
    }
}

async fn read_control(mut recv: RecvStream, mut buffer: BytesMut) -> Result<()> {
    loop {
        match read_packet(&mut recv, &mut buffer).await? {
            Packet::Disconnect(disconnect) => {
                bail!("broker sent DISCONNECT {:?}", disconnect.reason_code)
            }
            Packet::PingResp => {}
            Packet::Publish(publish) => bail!(
                "broker delivered {} on the control stream instead of its subscription stream",
                publish.topic_name
            ),
            other => bail!("unexpected packet on the control stream: {other:?}"),
        }
    }
}

async fn read_deliveries(
    mut recv: RecvStream,
    mut buffer: BytesMut,
    subscription: Subscription,
    conn_idx: u16,
    recorder: Arc<HolRecorder>,
) -> Result<()> {
    let stream_id = u64::from(recv.id());
    let num_topics = recorder.topic_samples.len();
    let filter = subscription.filter;
    loop {
        let publish = match read_packet(&mut recv, &mut buffer).await? {
            Packet::Publish(publish) => publish,
            other => bail!("unexpected packet on subscription stream {stream_id}: {other:?}"),
        };
        let topic = &publish.topic_name;
        let index = match subscription.topic_idx {
            Some(index) if *topic == filter => index,
            Some(_) => bail!("subscription stream {stream_id} for {filter} delivered {topic}"),
            None => match hol_topic_index(topic).filter(|index| *index < num_topics) {
                Some(index) => index,
                None => bail!(
                    "subscription stream {stream_id} for {filter} delivered {topic}, outside {HOL_TOPIC_PREFIX}0 to {HOL_TOPIC_PREFIX}{}",
                    num_topics.saturating_sub(1)
                ),
            },
        };
        recorder.record(index, conn_idx, &publish.payload, Some(stream_id));
    }
}
