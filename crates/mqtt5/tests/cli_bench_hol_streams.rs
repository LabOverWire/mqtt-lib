#![cfg(all(feature = "broker", feature = "transport-quic"))]

mod common;
use bytes::BytesMut;
use common::cli_helpers::{run_cli_command, CliResult};
use mqtt5::broker::quic_acceptor::QuicAcceptorConfig;
use mqtt5::packet::connack::ConnAckPacket;
use mqtt5::packet::suback::{SubAckPacket, SubAckReasonCode};
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::transport::packet_io::{encode_packet_to_buffer, read_packet_from_stream};
use mqtt5::{topic_matches_filter, Packet};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

type SharedSend = Arc<Mutex<quinn::SendStream>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    Serve,
    Refuse,
    EndAfterSubAck,
    ServerUniStream,
    ServerBiStream,
    Silent,
}
type Routes = Arc<Mutex<Vec<(String, u64, SharedSend, quinn::Connection)>>>;

struct FakeQuicBroker {
    url: String,
    routes: Routes,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for FakeQuicBroker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn read(recv: &mut quinn::RecvStream, buffer: &mut BytesMut) -> Option<Packet> {
    read_packet_from_stream(recv, 5, buffer, 1024 * 1024)
        .await
        .ok()
}

async fn write(send: &Mutex<quinn::SendStream>, packet: &Packet) -> bool {
    let mut buf = BytesMut::new();
    encode_packet_to_buffer(packet, &mut buf).unwrap();
    send.lock().await.write_all(&buf).await.is_ok()
}

async fn serve_subscription_stream(
    send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    conn: quinn::Connection,
    routes: Routes,
    behaviour: Behaviour,
) {
    let mut buffer = BytesMut::new();
    let Some(Packet::Subscribe(subscribe)) = read(&mut recv, &mut buffer).await else {
        return;
    };
    let code = if behaviour == Behaviour::Refuse {
        SubAckReasonCode::NotAuthorized
    } else {
        SubAckReasonCode::GrantedQoS0
    };
    let send = Arc::new(Mutex::new(send));
    let suback = SubAckPacket::new(subscribe.packet_id).add_reason_code(code);
    write(&send, &Packet::SubAck(suback)).await;
    if behaviour == Behaviour::EndAfterSubAck {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let _ = send.lock().await.finish();
        return;
    }
    if matches!(
        behaviour,
        Behaviour::Serve | Behaviour::ServerUniStream | Behaviour::ServerBiStream
    ) {
        routes.lock().await.push((
            subscribe.filters[0].filter.clone(),
            u64::from(recv.id()),
            Arc::clone(&send),
            conn,
        ));
    }
    while read(&mut recv, &mut buffer).await.is_some() {}
}

async fn serve_connection(conn: quinn::Connection, routes: Routes, behaviour: Behaviour) {
    let Ok((ctl_send, mut ctl_recv)) = conn.accept_bi().await else {
        return;
    };
    let ctl_send = Mutex::new(ctl_send);
    let mut buffer = BytesMut::new();
    let Some(Packet::Connect(_)) = read(&mut ctl_recv, &mut buffer).await else {
        return;
    };
    let connack = ConnAckPacket::new(false, ReasonCode::Success);
    write(&ctl_send, &Packet::ConnAck(connack)).await;

    let stream_conn = conn.clone();
    let stream_routes = Arc::clone(&routes);
    tokio::spawn(async move {
        while let Ok((send, recv)) = stream_conn.accept_bi().await {
            tokio::spawn(serve_subscription_stream(
                send,
                recv,
                stream_conn.clone(),
                Arc::clone(&stream_routes),
                behaviour,
            ));
        }
    });

    while let Some(packet) = read(&mut ctl_recv, &mut buffer).await {
        match packet {
            Packet::Publish(publish) => {
                let targets: Vec<(SharedSend, quinn::Connection)> = routes
                    .lock()
                    .await
                    .iter()
                    .filter(|(filter, _, _, _)| topic_matches_filter(&publish.topic_name, filter))
                    .map(|(_, _, send, subscriber)| (Arc::clone(send), subscriber.clone()))
                    .collect();
                if targets.is_empty() {
                    continue;
                }
                let subscriber = targets[0].1.clone();
                match behaviour {
                    Behaviour::ServerUniStream => {
                        if let Ok(uni) = subscriber.open_uni().await {
                            let uni = Mutex::new(uni);
                            write(&uni, &Packet::Publish(publish.clone())).await;
                            let _ = uni.lock().await.finish();
                        }
                        continue;
                    }
                    Behaviour::ServerBiStream => {
                        if let Ok((bi, _)) = subscriber.open_bi().await {
                            let bi = Mutex::new(bi);
                            write(&bi, &Packet::Publish(publish.clone())).await;
                            let _ = bi.lock().await.finish();
                        }
                        continue;
                    }
                    _ => {}
                }
                for (target, _) in targets {
                    write(&target, &Packet::Publish(publish.clone())).await;
                }
            }
            Packet::PingReq => {
                write(&ctl_send, &Packet::PingResp).await;
            }
            _ => {}
        }
    }
}

async fn fake_quic_broker(behaviour: Behaviour) -> FakeQuicBroker {
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
    let routes: Routes = Arc::new(Mutex::new(Vec::new()));
    let accept_routes = Arc::clone(&routes);
    let task = tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let routes = Arc::clone(&accept_routes);
            tokio::spawn(async move {
                if let Ok(conn) = incoming.await {
                    serve_connection(conn, routes, behaviour).await;
                }
            });
        }
    });
    FakeQuicBroker { url, routes, task }
}

async fn run_hol_streams(url: &str, extra: &[&str]) -> CliResult {
    let mut args = vec![
        "bench",
        "--url",
        url,
        "--mode",
        "hol-blocking",
        "--duration",
        "1",
        "--warmup",
        "0",
        "--topics",
        "4",
        "--sub-connections",
        "2",
        "--rate",
        "400",
        "--hol-subscription-streams",
    ];
    args.extend_from_slice(extra);
    run_cli_command(&args).await
}

fn trace_rows(trace_dir: &Path) -> (String, Vec<Vec<u64>>) {
    let csv = std::fs::read_to_string(trace_dir.join("messages.csv")).unwrap();
    let mut lines = csv.lines();
    let header = lines.next().unwrap().to_string();
    let rows = lines
        .map(|line| line.split(',').map(|v| v.parse().unwrap()).collect())
        .collect();
    (header, rows)
}

async fn routed_filters(broker: &FakeQuicBroker) -> Vec<(String, u64)> {
    let mut routes: Vec<(String, u64)> = broker
        .routes
        .lock()
        .await
        .iter()
        .map(|(filter, stream_id, _, _)| (filter.clone(), *stream_id))
        .collect();
    routes.sort();
    routes
}

#[tokio::test]
async fn each_topic_subscribes_and_is_delivered_on_its_own_stream() {
    let broker = fake_quic_broker(Behaviour::Serve).await;
    let trace_dir = tempfile::tempdir().unwrap();
    let result = run_hol_streams(
        &broker.url,
        &["--trace-dir", trace_dir.path().to_str().unwrap()],
    )
    .await;
    assert!(result.success, "bench failed: {}", result.stderr);
    let json: serde_json::Value = serde_json::from_str(result.stdout.trim()).unwrap();
    assert_eq!(json["config"]["hol_subscription_streams"], true);

    let routes = routed_filters(&broker).await;
    let filters: Vec<&str> = routes.iter().map(|(filter, _)| filter.as_str()).collect();
    assert_eq!(
        filters,
        ["bench/hol/0", "bench/hol/1", "bench/hol/2", "bench/hol/3"]
    );

    let (header, rows) = trace_rows(trace_dir.path());
    assert_eq!(
        header,
        "topic_idx,seq,publish_ns,receive_ns,latency_us,stream_id,conn_idx"
    );
    let mut delivered = [0u32; 4];
    for row in &rows {
        let topic_idx = usize::try_from(row[0]).unwrap();
        assert_eq!(row[5], routes[topic_idx].1, "topic {topic_idx}: {row:?}");
        assert_eq!(
            row[6],
            row[0] % 2,
            "topic {topic_idx} on the wrong connection"
        );
        delivered[topic_idx] += 1;
    }
    assert!(
        delivered.iter().all(|&count| count > 0),
        "every topic must be delivered: {delivered:?}"
    );
    let distinct_on_conn0 = routes[0].1 != routes[2].1;
    let distinct_on_conn1 = routes[1].1 != routes[3].1;
    assert!(distinct_on_conn0 && distinct_on_conn1, "{routes:?}");
}

#[tokio::test]
async fn wildcard_subscribes_once_per_connection_on_one_stream() {
    let broker = fake_quic_broker(Behaviour::Serve).await;
    let trace_dir = tempfile::tempdir().unwrap();
    let result = run_hol_streams(
        &broker.url,
        &[
            "--hol-wildcard",
            "--trace-dir",
            trace_dir.path().to_str().unwrap(),
        ],
    )
    .await;
    assert!(result.success, "bench failed: {}", result.stderr);

    let routes = routed_filters(&broker).await;
    assert_eq!(routes.len(), 2, "{routes:?}");
    assert!(routes.iter().all(|(filter, _)| filter == "bench/hol/#"));

    let (_, rows) = trace_rows(trace_dir.path());
    let mut topics_seen = [false; 4];
    for row in &rows {
        topics_seen[usize::try_from(row[0]).unwrap()] = true;
        assert!(
            routes.iter().any(|(_, stream_id)| *stream_id == row[5]),
            "{row:?}"
        );
    }
    assert_eq!(topics_seen, [true; 4]);
}

#[tokio::test]
async fn refused_subscription_fails_the_run() {
    let broker = fake_quic_broker(Behaviour::Refuse).await;
    let result = run_hol_streams(&broker.url, &[]).await;
    assert!(!result.success, "bench must fail: {}", result.stdout);
    assert!(
        result.stderr.contains("refused"),
        "stderr must name the refusal: {}",
        result.stderr
    );
}

#[tokio::test]
async fn subscription_stream_that_ends_early_fails_the_run() {
    let broker = fake_quic_broker(Behaviour::EndAfterSubAck).await;
    let result = run_hol_streams(&broker.url, &[]).await;
    assert!(!result.success, "bench must fail: {}", result.stdout);
    assert!(
        result.stderr.contains("ended before the run finished"),
        "stderr must name the stream that ended: {}",
        result.stderr
    );
}

async fn assert_server_stream_fails_the_run(behaviour: Behaviour) {
    let broker = fake_quic_broker(behaviour).await;
    let result = run_hol_streams(&broker.url, &[]).await;
    assert!(!result.success, "bench must fail: {}", result.stdout);
    assert!(
        result.stderr.contains("broker opened stream"),
        "stderr must name the server stream: {}",
        result.stderr
    );
}

#[tokio::test]
async fn delivery_on_a_server_uni_stream_fails_the_run() {
    assert_server_stream_fails_the_run(Behaviour::ServerUniStream).await;
}

#[tokio::test]
async fn delivery_on_a_server_bi_stream_fails_the_run() {
    assert_server_stream_fails_the_run(Behaviour::ServerBiStream).await;
}

#[tokio::test]
async fn run_without_deliveries_fails() {
    let broker = fake_quic_broker(Behaviour::Silent).await;
    let result = run_hol_streams(&broker.url, &[]).await;
    assert!(!result.success, "bench must fail: {}", result.stdout);
    assert!(
        result.stderr.contains("received no messages"),
        "{}",
        result.stderr
    );
}

#[tokio::test]
async fn subscription_streams_refuse_qos_above_zero() {
    let broker = fake_quic_broker(Behaviour::Serve).await;
    let result = run_hol_streams(&broker.url, &["--qos", "1"]).await;
    assert!(
        !result.success,
        "bench must refuse --qos 1: {}",
        result.stdout
    );
    assert!(
        result
            .stderr
            .contains("--hol-subscription-streams subscribes at QoS 0 only"),
        "{}",
        result.stderr
    );
    assert!(routed_filters(&broker).await.is_empty());
}

#[tokio::test]
async fn subscription_streams_need_a_quic_url_and_hol_mode() {
    let tcp = run_hol_streams("mqtt://127.0.0.1:9", &[]).await;
    assert!(!tcp.success);
    assert!(
        tcp.stderr.contains("needs a quic:// or quics:// URL"),
        "{}",
        tcp.stderr
    );

    let wrong_mode = run_cli_command(&[
        "bench",
        "--url",
        "quic://127.0.0.1:9",
        "--mode",
        "throughput",
        "--hol-subscription-streams",
    ])
    .await;
    assert!(!wrong_mode.success);
    assert!(
        wrong_mode
            .stderr
            .contains("--hol-subscription-streams needs --mode hol-blocking"),
        "{}",
        wrong_mode.stderr
    );
}
