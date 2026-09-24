#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::quic_acceptor::QuicAcceptorConfig;
use mqtt5::{ConnectOptions, Message, MqttClient, QoS, SubscribeOptions};
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc;

const SUBSCRIBE: u8 = 0x82;
const PUBACK: u8 = 0x40;
const DISCONNECT: u8 = 0xE0;
const UNSPECIFIED_CLOSE: u32 = 0xB2;

struct FakeBroker {
    _endpoint: quinn::Endpoint,
    conn: quinn::Connection,
    ctl_send: quinn::SendStream,
    ctl_recv: quinn::RecvStream,
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

fn encode_varint(mut value: usize, out: &mut Vec<u8>) {
    loop {
        let mut byte = u8::try_from(value % 128).expect("remainder fits u8");
        value /= 128;
        if value > 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn publish_frame(
    qos: u8,
    topic: &str,
    packet_id: Option<u16>,
    props: &[u8],
    payload: &[u8],
) -> Vec<u8> {
    let mut body = u16::try_from(topic.len())
        .expect("topic fits u16")
        .to_be_bytes()
        .to_vec();
    body.extend_from_slice(topic.as_bytes());
    if let Some(id) = packet_id {
        body.extend_from_slice(&id.to_be_bytes());
    }
    encode_varint(props.len(), &mut body);
    body.extend_from_slice(props);
    body.extend_from_slice(payload);
    let mut frame = vec![0x30 | (qos << 1)];
    encode_varint(body.len(), &mut frame);
    frame.extend_from_slice(&body);
    frame
}

fn topic_alias_prop(alias: u16) -> Vec<u8> {
    let mut out = vec![0x23];
    out.extend_from_slice(&alias.to_be_bytes());
    out
}

async fn connect_fake_broker(options: ConnectOptions) -> (MqttClient, FakeBroker) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs");
    let certs = QuicAcceptorConfig::load_cert_chain_from_file(cert_dir.join("server.pem"))
        .await
        .expect("server cert");
    let key = QuicAcceptorConfig::load_private_key_from_file(cert_dir.join("server.key"))
        .await
        .expect("server key");
    let server_config = QuicAcceptorConfig::new(certs, key)
        .with_alpn_protocols(vec![b"mqtt".to_vec()])
        .build_server_config()
        .expect("server config");
    let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap())
        .expect("server endpoint");
    let url = format!("quic://{}", endpoint.local_addr().expect("local addr"));

    let client = MqttClient::with_options(options.clone());
    client.set_insecure_tls(true).await;
    let connecting = client.clone();
    let connect =
        tokio::spawn(async move { Box::pin(connecting.connect_with_options(&url, options)).await });

    let conn = endpoint
        .accept()
        .await
        .expect("incoming connection")
        .await
        .expect("handshake");
    let (mut ctl_send, mut ctl_recv) = conn.accept_bi().await.expect("control stream");
    let connect_frame = read_frame(&mut ctl_recv).await.expect("CONNECT");
    assert_eq!(connect_frame[0] >> 4, 1, "first packet must be CONNECT");
    ctl_send
        .write_all(&[0x20, 0x03, 0x00, 0x00, 0x00])
        .await
        .expect("write CONNACK");
    connect
        .await
        .expect("connect task")
        .expect("client connects to fake broker");
    (
        client,
        FakeBroker {
            _endpoint: endpoint,
            conn,
            ctl_send,
            ctl_recv,
        },
    )
}

async fn subscribe(
    client: &MqttClient,
    broker: &mut FakeBroker,
    filter: &str,
    qos: QoS,
) -> mpsc::UnboundedReceiver<Message> {
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
    let frame = read_frame(&mut broker.ctl_recv).await.expect("SUBSCRIBE");
    assert_eq!(frame[0], SUBSCRIBE);
    broker
        .ctl_send
        .write_all(&[0x90, 0x04, frame[2], frame[3], 0x00, qos as u8])
        .await
        .expect("write SUBACK");
    task.await.expect("subscribe task").expect("subscribe");
    rx
}

async fn drain(rx: &mut mpsc::UnboundedReceiver<Message>, wait: Duration) -> Vec<Message> {
    let mut messages = Vec::new();
    while let Ok(Some(message)) = tokio::time::timeout(wait, rx.recv()).await {
        messages.push(message);
    }
    messages
}

async fn expect_disconnect_and_close(broker: &mut FakeBroker, reason: u8) {
    let frame = tokio::time::timeout(Duration::from_secs(3), read_frame(&mut broker.ctl_recv))
        .await
        .expect("client answers within 3s");
    let frame = frame.expect("DISCONNECT on the control stream");
    assert_eq!(frame[0], DISCONNECT, "client must send DISCONNECT");
    assert_eq!(frame[2], reason, "DISCONNECT reason code");
    let closed = tokio::time::timeout(Duration::from_secs(3), broker.conn.closed()).await;
    match closed {
        Ok(quinn::ConnectionError::ApplicationClosed(close)) => assert_eq!(
            close.error_code.into_inner(),
            u64::from(UNSPECIFIED_CLOSE),
            "error teardown must close with an application error code"
        ),
        other => panic!("[MQTT-3.14.4-2] QUIC connection not closed by the client: {other:?}"),
    }
}

#[tokio::test]
async fn mqtt_3_14_4_1_quic_protocol_error_closes_connection_and_stops_acking() {
    let options = ConnectOptions::new("conf-quic-close").with_automatic_reconnect(false);
    let (client, mut broker) = connect_fake_broker(options).await;
    let mut rx = subscribe(&client, &mut broker, "t", QoS::AtLeastOnce).await;

    broker
        .ctl_send
        .write_all(&[0xD1, 0x00])
        .await
        .expect("write malformed PINGRESP");
    expect_disconnect_and_close(&mut broker, 0x81).await;
    assert!(!client.is_connected().await);

    let acked_after_disconnect = match broker.conn.open_bi().await {
        Ok((mut data_send, mut data_recv)) => {
            let _ = data_send
                .write_all(&publish_frame(1, "t", Some(7), &[], b"x"))
                .await;
            let next =
                tokio::time::timeout(Duration::from_secs(2), read_frame(&mut data_recv)).await;
            matches!(next, Ok(Some(frame)) if frame[0] == PUBACK)
        }
        Err(_) => false,
    };
    assert!(
        !acked_after_disconnect,
        "[MQTT-3.14.4-1] client wrote PUBACK after its own DISCONNECT"
    );
    assert!(
        drain(&mut rx, Duration::from_millis(300)).await.is_empty(),
        "[MQTT-3.14.4-1] client delivered a PUBLISH received after its own DISCONNECT"
    );

    let _ = client.disconnect().await;
    assert!(
        client.quic_connection().await.is_none(),
        "disconnect() after a reader-initiated close must release the QUIC connection"
    );
}

#[tokio::test]
async fn mqtt_3_1_2_24_quic_control_stream_enforces_client_maximum_packet_size() {
    let mut options = ConnectOptions::new("conf-quic-mps-ctl").with_automatic_reconnect(false);
    options.properties.maximum_packet_size = Some(64);
    let (client, mut broker) = connect_fake_broker(options).await;
    let mut rx = subscribe(&client, &mut broker, "t", QoS::AtMostOnce).await;

    broker
        .ctl_send
        .write_all(&publish_frame(0, "t", None, &[], &[b'x'; 200]))
        .await
        .expect("write oversized PUBLISH");
    expect_disconnect_and_close(&mut broker, 0x95).await;
    assert!(
        drain(&mut rx, Duration::from_millis(300)).await.is_empty(),
        "[MQTT-3.1.2-24] oversized PUBLISH on the control stream was delivered"
    );
}

#[tokio::test]
async fn mqtt_3_1_2_24_quic_data_stream_enforces_client_maximum_packet_size() {
    let mut options = ConnectOptions::new("conf-quic-mps-data").with_automatic_reconnect(false);
    options.properties.maximum_packet_size = Some(64);
    let (client, mut broker) = connect_fake_broker(options).await;
    let mut rx = subscribe(&client, &mut broker, "t", QoS::AtMostOnce).await;

    let (mut data_send, _data_recv) = broker.conn.open_bi().await.expect("open data stream");
    data_send
        .write_all(&publish_frame(0, "t", None, &[], &[b'x'; 200]))
        .await
        .expect("write oversized PUBLISH");
    expect_disconnect_and_close(&mut broker, 0x95).await;
    assert!(
        drain(&mut rx, Duration::from_millis(300)).await.is_empty(),
        "[MQTT-3.1.2-24] oversized PUBLISH on a data stream was delivered"
    );
}

#[tokio::test]
async fn mqtt_3_3_2_10_quic_unidirectional_stream_resolves_topic_alias() {
    let mut options = ConnectOptions::new("conf-quic-uni-alias").with_automatic_reconnect(false);
    options.properties.topic_alias_maximum = Some(2);
    let (client, mut broker) = connect_fake_broker(options).await;
    let mut rx = subscribe(&client, &mut broker, "t/a", QoS::AtMostOnce).await;

    let mut uni = broker.conn.open_uni().await.expect("open uni stream");
    uni.write_all(&publish_frame(0, "t/a", None, &topic_alias_prop(1), b"one"))
        .await
        .expect("write aliased PUBLISH");
    uni.write_all(&publish_frame(0, "", None, &topic_alias_prop(1), b"two"))
        .await
        .expect("write alias-only PUBLISH");
    let topics: Vec<String> = drain(&mut rx, Duration::from_millis(600))
        .await
        .into_iter()
        .map(|m| m.topic)
        .collect();
    assert_eq!(
        topics,
        vec!["t/a", "t/a"],
        "[MQTT-3.3.2-10] inbound Topic Alias on a QUIC unidirectional stream was not resolved"
    );
}

async fn publish_acknowledged_on_server_stream(
    level: QoS,
    ack_type: u8,
) -> (
    tokio::task::JoinHandle<mqtt5::Result<mqtt5::PublishResult>>,
    FakeBroker,
) {
    let options = ConnectOptions::new("conf-quic-data-ack").with_automatic_reconnect(false);
    let (client, mut broker) = connect_fake_broker(options).await;
    let publisher = client.clone();
    let publishing =
        tokio::spawn(async move { publisher.publish_qos("t/ack", b"x".to_vec(), level).await });
    let frame = read_frame(&mut broker.ctl_recv).await.expect("PUBLISH");
    assert_eq!(
        frame[0] >> 4,
        3,
        "client must send its PUBLISH on the control stream"
    );
    let topic_len = usize::from(u16::from_be_bytes([frame[2], frame[3]]));
    let pid = [frame[4 + topic_len], frame[5 + topic_len]];
    let (mut data_send, _data_recv) = broker.conn.open_bi().await.expect("open data stream");
    data_send
        .write_all(&[ack_type, 0x02, pid[0], pid[1]])
        .await
        .expect("write acknowledgement on a server-opened stream");
    (publishing, broker)
}

#[tokio::test]
async fn quic_puback_on_server_opened_stream_settles_the_publish() {
    let (publishing, _broker) =
        publish_acknowledged_on_server_stream(QoS::AtLeastOnce, PUBACK).await;
    let result = tokio::time::timeout(Duration::from_secs(3), publishing)
        .await
        .expect("publish settles within 3s")
        .expect("publish task");
    assert!(
        matches!(
            result,
            Ok(mqtt5::PublishResult::Sent(
                mqtt5::Delivery::AtLeastOnce { .. }
            ))
        ),
        "a PUBACK on a server-opened QUIC stream must settle the publish as delivered: {result:?}"
    );
}

#[tokio::test]
async fn quic_mismatched_ack_on_server_opened_stream_is_a_protocol_error() {
    let (publishing, mut broker) =
        publish_acknowledged_on_server_stream(QoS::ExactlyOnce, PUBACK).await;
    expect_disconnect_and_close(&mut broker, 0x82).await;
    let result = tokio::time::timeout(Duration::from_secs(3), publishing)
        .await
        .expect("publish returns within 3s")
        .expect("publish task");
    assert!(
        !matches!(result, Ok(mqtt5::PublishResult::Sent(_))),
        "a PUBACK for a QoS 2 publish must not be accepted as its acknowledgement: {result:?}"
    );
}
