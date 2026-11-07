#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::quic_acceptor::QuicAcceptorConfig;
use mqtt5::{ConnectOptions, Message, MqttClient, QoS, SubscribeOptions};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

const SUBSCRIBE: u8 = 0x82;
const UNSUBSCRIBE: u8 = 0xA2;
const PUBACK: u8 = 0x40;

struct FakeBroker {
    endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    ctl_recv: quinn::RecvStream,
    ctl_send: quinn::SendStream,
}

async fn read_frame(recv: &mut quinn::RecvStream) -> Option<Vec<u8>> {
    let mut first = [0u8; 1];
    recv.read_exact(&mut first).await.ok()?;
    let mut frame = vec![first[0]];
    let mut remaining = 0usize;
    let mut multiplier = 1usize;
    loop {
        let mut byte = [0u8; 1];
        recv.read_exact(&mut byte).await.ok()?;
        frame.push(byte[0]);
        remaining += usize::from(byte[0] & 0x7F) * multiplier;
        multiplier *= 128;
        if byte[0] & 0x80 == 0 {
            break;
        }
    }
    let mut body = vec![0u8; remaining];
    recv.read_exact(&mut body).await.ok()?;
    frame.extend_from_slice(&body);
    Some(frame)
}

fn publish_frame(qos: u8, topic: &str, packet_id: Option<u16>, payload: &[u8]) -> Vec<u8> {
    let mut body = u16::try_from(topic.len()).unwrap().to_be_bytes().to_vec();
    body.extend_from_slice(topic.as_bytes());
    if let Some(id) = packet_id {
        body.extend_from_slice(&id.to_be_bytes());
    }
    body.push(0x00);
    body.extend_from_slice(payload);
    let mut frame = vec![0x30 | (qos << 1)];
    frame.push(u8::try_from(body.len()).unwrap());
    frame.extend_from_slice(&body);
    frame
}

async fn accept_session(endpoint: &quinn::Endpoint) -> FakeBroker {
    let conn = endpoint.accept().await.unwrap().await.unwrap();
    let (mut ctl_send, mut ctl_recv) = conn.accept_bi().await.unwrap();
    let connect_frame = read_frame(&mut ctl_recv).await.unwrap();
    assert_eq!(connect_frame[0] >> 4, 1);
    ctl_send
        .write_all(&[0x20, 0x03, 0x00, 0x00, 0x00])
        .await
        .unwrap();
    FakeBroker {
        endpoint: endpoint.clone(),
        conn,
        ctl_recv,
        ctl_send,
    }
}

async fn connect_fake_broker(
    client_id: &str,
    subscription_streams: bool,
) -> (MqttClient, FakeBroker) {
    connect_fake_broker_with(
        ConnectOptions::new(client_id).with_automatic_reconnect(false),
        subscription_streams,
    )
    .await
}

async fn connect_fake_broker_with(
    options: ConnectOptions,
    subscription_streams: bool,
) -> (MqttClient, FakeBroker) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs");
    let certs = QuicAcceptorConfig::load_cert_chain_from_file(cert_dir.join("server.pem"))
        .await
        .unwrap();
    let key = QuicAcceptorConfig::load_private_key_from_file(cert_dir.join("server.key"))
        .await
        .unwrap();
    let server_config = QuicAcceptorConfig::new(certs, key)
        .with_alpn_protocols(vec![b"mqtt".to_vec()])
        .build_server_config()
        .unwrap();
    let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("quic://{}", endpoint.local_addr().unwrap());

    let client = MqttClient::with_options(options.clone());
    client.set_insecure_tls(true).await;
    client
        .set_quic_subscription_streams(subscription_streams)
        .await;
    let connecting = client.clone();
    let connect =
        tokio::spawn(async move { Box::pin(connecting.connect_with_options(&url, options)).await });

    let broker = accept_session(&endpoint).await;
    connect.await.unwrap().unwrap();
    (client, broker)
}

fn start_subscribe(
    client: &MqttClient,
    filter: &str,
    qos: QoS,
) -> (
    tokio::task::JoinHandle<mqtt5::Result<(u16, QoS)>>,
    mpsc::UnboundedReceiver<Message>,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let subscriber = client.clone();
    let filter = filter.to_string();
    let options = SubscribeOptions {
        qos,
        ..Default::default()
    };
    let task = tokio::spawn(async move {
        subscriber
            .subscribe_with_options(filter, options, move |message| {
                let _ = tx.send(message);
            })
            .await
    });
    (task, rx)
}

async fn nothing_on(recv: &mut quinn::RecvStream) -> bool {
    tokio::time::timeout(Duration::from_millis(300), read_frame(recv))
        .await
        .is_err()
}

#[tokio::test]
async fn subscribe_travels_on_its_own_stream_and_deliveries_return_on_it() {
    let (client, mut broker) = connect_fake_broker("sub-streams-deliver", true).await;
    let (task, mut rx) = start_subscribe(&client, "mirror/a", QoS::AtLeastOnce);

    let (mut data_send, mut data_recv) = broker.conn.accept_bi().await.unwrap();
    let data_stream_id = u64::from(data_recv.id());
    let subscribe = read_frame(&mut data_recv).await.unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    data_send
        .write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x01])
        .await
        .unwrap();
    task.await.unwrap().unwrap();
    assert!(nothing_on(&mut broker.ctl_recv).await);

    data_send
        .write_all(&publish_frame(0, "mirror/a", None, b"q0"))
        .await
        .unwrap();
    data_send
        .write_all(&publish_frame(1, "mirror/a", Some(9), b"q1"))
        .await
        .unwrap();

    let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.payload, b"q0");
    assert_eq!(second.payload, b"q1");
    assert_eq!(first.stream_id, Some(data_stream_id));
    assert_eq!(second.stream_id, Some(data_stream_id));

    let puback = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut data_recv))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(puback[0], PUBACK);
    assert_eq!(&puback[2..4], &9u16.to_be_bytes());
    assert!(nothing_on(&mut broker.ctl_recv).await);
}

#[tokio::test]
async fn each_subscription_gets_a_distinct_stream_and_unsubscribe_closes_it() {
    let (client, mut broker) = connect_fake_broker("sub-streams-unsub", true).await;
    let mut streams = Vec::new();
    for filter in ["mirror/x", "mirror/y"] {
        let (task, _rx) = start_subscribe(&client, filter, QoS::AtMostOnce);
        let (mut send, mut recv) = broker.conn.accept_bi().await.unwrap();
        let subscribe = read_frame(&mut recv).await.unwrap();
        assert_eq!(subscribe[0], SUBSCRIBE);
        send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
            .await
            .unwrap();
        task.await.unwrap().unwrap();
        streams.push((send, recv));
    }
    assert_ne!(streams[0].1.id(), streams[1].1.id());

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("mirror/x").await });
    let (send, recv) = &mut streams[0];
    let frame = read_frame(recv).await.unwrap();
    assert_eq!(frame[0], UNSUBSCRIBE);
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    unsubscribe.await.unwrap().unwrap();

    let after = tokio::time::timeout(Duration::from_secs(2), read_frame(recv))
        .await
        .unwrap();
    assert!(after.is_none(), "unsubscribed stream must be finished");
    assert!(nothing_on(&mut streams[1].1).await);
    assert!(nothing_on(&mut broker.ctl_recv).await);
}

#[tokio::test]
async fn subscribe_stays_on_the_control_stream_by_default() {
    let (client, mut broker) = connect_fake_broker("sub-streams-default", false).await;
    let (task, _rx) = start_subscribe(&client, "plain/a", QoS::AtMostOnce);
    let subscribe = read_frame(&mut broker.ctl_recv).await.unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    broker
        .ctl_send
        .write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();
}

async fn accept_subscription_stream(
    conn: &quinn::Connection,
    granted: u8,
) -> (quinn::SendStream, quinn::RecvStream) {
    let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(5), conn.accept_bi())
        .await
        .unwrap()
        .unwrap();
    let subscribe = read_frame(&mut recv).await.unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, granted])
        .await
        .unwrap();
    (send, recv)
}

#[tokio::test]
async fn concurrent_subscribes_to_one_filter_keep_a_single_stream_open() {
    let (client, broker) = connect_fake_broker("sub-streams-concurrent", true).await;
    let (mut first_task, _first_rx) = start_subscribe(&client, "mirror/dup", QoS::AtMostOnce);
    let (mut second_task, _second_rx) = start_subscribe(&client, "mirror/dup", QoS::AtMostOnce);

    let (mut first_send, mut first_recv) = broker.conn.accept_bi().await.unwrap();
    let (mut second_send, mut second_recv) = broker.conn.accept_bi().await.unwrap();
    let first_subscribe = read_frame(&mut first_recv).await.unwrap();
    let second_subscribe = read_frame(&mut second_recv).await.unwrap();
    assert_eq!(first_subscribe[0], SUBSCRIBE);
    assert_eq!(second_subscribe[0], SUBSCRIBE);
    first_send
        .write_all(&[
            0x90,
            0x04,
            first_subscribe[2],
            first_subscribe[3],
            0x00,
            0x00,
        ])
        .await
        .unwrap();
    let remaining = tokio::select! {
        settled = &mut first_task => {
            settled.unwrap().unwrap();
            second_task
        }
        settled = &mut second_task => {
            settled.unwrap().unwrap();
            first_task
        }
    };
    second_send
        .write_all(&[
            0x90,
            0x04,
            second_subscribe[2],
            second_subscribe[3],
            0x00,
            0x00,
        ])
        .await
        .unwrap();
    remaining.await.unwrap().unwrap();

    let first_after = tokio::time::timeout(Duration::from_secs(1), read_frame(&mut first_recv))
        .await
        .ok();
    let second_after = tokio::time::timeout(Duration::from_secs(1), read_frame(&mut second_recv))
        .await
        .ok();
    assert_eq!(
        first_after,
        Some(None),
        "the stream displaced by the later SUBACK must be finished"
    );
    assert_eq!(
        second_after, None,
        "the stream that owns the filter must stay open"
    );
}

#[tokio::test]
async fn resubscribe_after_reconnect_opens_a_stream_on_the_new_connection() {
    let options = ConnectOptions::new("sub-streams-reconnect")
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(50), Duration::from_millis(100));
    let (client, broker) = connect_fake_broker_with(options, true).await;
    let (task, mut rx) = start_subscribe(&client, "mirror/r", QoS::AtMostOnce);
    accept_subscription_stream(&broker.conn, 0x00).await;
    task.await.unwrap().unwrap();

    broker.conn.close(0u32.into(), b"restart");
    let reconnected =
        tokio::time::timeout(Duration::from_secs(5), accept_session(&broker.endpoint))
            .await
            .unwrap();
    let (mut send, recv) = accept_subscription_stream(&reconnected.conn, 0x00).await;
    let new_stream_id = u64::from(recv.id());

    send.write_all(&publish_frame(0, "mirror/r", None, b"after"))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.payload, b"after");
    assert_eq!(message.stream_id, Some(new_stream_id));
}

#[tokio::test]
async fn refused_subscription_finishes_its_stream() {
    let (client, broker) = connect_fake_broker("sub-streams-refused", true).await;
    let (task, _rx) = start_subscribe(&client, "mirror/denied", QoS::AtMostOnce);
    let (_send, mut recv) = accept_subscription_stream(&broker.conn, 0x80).await;
    assert!(task.await.unwrap().is_err());
    let after = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .unwrap();
    assert!(
        after.is_none(),
        "refused subscription stream must be finished"
    );
}

#[tokio::test]
async fn publish_on_a_server_stream_without_flow_header_reports_the_stream_id() {
    let (client, mut broker) = connect_fake_broker("sub-streams-server-stream", false).await;
    let (task, mut rx) = start_subscribe(&client, "plain/s", QoS::AtMostOnce);
    let subscribe = read_frame(&mut broker.ctl_recv).await.unwrap();
    broker
        .ctl_send
        .write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();

    let (mut send, _recv) = broker.conn.open_bi().await.unwrap();
    let server_stream_id = u64::from(send.id());
    send.write_all(&publish_frame(0, "plain/s", None, b"srv"))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.payload, b"srv");
    assert_eq!(message.stream_id, Some(server_stream_id));
}

#[tokio::test]
async fn filter_whose_stream_the_broker_reset_subscribes_on_a_new_stream() {
    let (client, broker) = connect_fake_broker("sub-streams-reset", true).await;
    let (task, _rx) = start_subscribe(&client, "mirror/reset", QoS::AtMostOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x00).await;
    task.await.unwrap().unwrap();

    send.reset(0u32.into()).unwrap();
    recv.stop(0u32.into()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(client.is_connected().await);

    let (task, _rx) = start_subscribe(&client, "mirror/reset", QoS::AtMostOnce);
    let (_send, recv) = accept_subscription_stream(&broker.conn, 0x00).await;
    assert_ne!(u64::from(recv.id()), u64::from(send.id()));
    task.await.unwrap().unwrap();
}
