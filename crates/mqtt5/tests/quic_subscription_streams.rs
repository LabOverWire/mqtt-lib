#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::quic_acceptor::QuicAcceptorConfig;
use mqtt5::{ConnectOptions, Message, MqttClient, MqttError, QoS, SubscribeOptions};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

const SUBSCRIBE: u8 = 0x82;
const UNSUBSCRIBE: u8 = 0xA2;
const PUBACK: u8 = 0x40;
const PUBREC: u8 = 0x50;
const PUBREL: u8 = 0x62;
const PUBCOMP: u8 = 0x70;

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
    connect_fake_broker_limited(options, subscription_streams, None).await
}

async fn connect_fake_broker_limited(
    options: ConnectOptions,
    subscription_streams: bool,
    transport: Option<quinn::TransportConfig>,
) -> (MqttClient, FakeBroker) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs");
    let certs = QuicAcceptorConfig::load_cert_chain_from_file(cert_dir.join("server.pem"))
        .await
        .unwrap();
    let key = QuicAcceptorConfig::load_private_key_from_file(cert_dir.join("server.key"))
        .await
        .unwrap();
    let mut server_config = QuicAcceptorConfig::new(certs, key)
        .with_alpn_protocols(vec![b"mqtt".to_vec()])
        .build_server_config()
        .unwrap();
    if let Some(transport) = transport {
        server_config.transport_config(std::sync::Arc::new(transport));
    }
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

    let broker = tokio::time::timeout(Duration::from_secs(5), accept_session(&endpoint))
        .await
        .unwrap();
    connect.await.unwrap().unwrap();
    (client, broker)
}

fn bidi_stream_limit(limit: u32) -> quinn::TransportConfig {
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_bidi_streams(limit.into());
    transport
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
async fn each_subscription_gets_a_distinct_stream_that_outlives_its_unsubscribe() {
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

    assert!(nothing_on(recv).await, "unsubscribed stream must stay open");
    assert!(nothing_on(&mut streams[1].1).await);
    assert!(nothing_on(&mut broker.ctl_recv).await);

    let (task, _rx) = start_subscribe(&client, "mirror/x", QoS::AtMostOnce);
    let (send, recv) = &mut streams[0];
    let subscribe = tokio::time::timeout(Duration::from_secs(2), read_frame(recv))
        .await
        .expect("resubscribe did not reuse the idle stream")
        .unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();
    assert!(nothing_on(&mut streams[1].1).await);
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
async fn concurrent_subscribes_to_one_filter_complete_and_keep_their_streams_open() {
    let (client, broker) = connect_fake_broker("sub-streams-concurrent", true).await;
    let (first_task, _first_rx) = start_subscribe(&client, "mirror/dup", QoS::AtMostOnce);
    let (second_task, _second_rx) = start_subscribe(&client, "mirror/dup", QoS::AtMostOnce);

    let (mut first_send, mut first_recv) = accept_subscription_stream(&broker.conn, 0x00).await;
    let mut second_stream = tokio::select! {
        frame = read_frame(&mut first_recv) => {
            let frame = frame.unwrap();
            assert_eq!(frame[0], SUBSCRIBE);
            first_send
                .write_all(&[0x90, 0x04, frame[2], frame[3], 0x00, 0x00])
                .await
                .unwrap();
            None
        }
        accepted = accept_subscription_stream(&broker.conn, 0x00) => Some(accepted),
        () = tokio::time::sleep(Duration::from_secs(5)) => panic!("second SUBSCRIBE never arrived"),
    };
    tokio::time::timeout(Duration::from_secs(2), first_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), second_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert!(nothing_on(&mut first_recv).await, "stream must stay open");
    if let Some((_send, recv)) = second_stream.as_mut() {
        assert!(nothing_on(recv).await, "stream must stay open");
    }
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
async fn refused_subscription_leaves_its_stream_open_and_idle() {
    let (client, broker) = connect_fake_broker("sub-streams-refused", true).await;
    let (task, _rx) = start_subscribe(&client, "mirror/denied", QoS::AtMostOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x80).await;
    assert!(task.await.unwrap().is_err());
    assert!(
        nothing_on(&mut recv).await,
        "refused subscription stream must stay open"
    );

    let (task, _rx) = start_subscribe(&client, "mirror/other", QoS::AtMostOnce);
    let subscribe = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .expect("SUBSCRIBE of another filter did not reuse the idle stream")
        .unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();
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

#[tokio::test]
async fn subscribe_fails_within_the_suback_limit_when_no_stream_is_available() {
    let options = ConnectOptions::new("sub-streams-no-stream").with_automatic_reconnect(false);
    let (client, broker) =
        connect_fake_broker_limited(options, true, Some(bidi_stream_limit(2))).await;
    let (task, _rx) = start_subscribe(&client, "limit/1", QoS::AtMostOnce);
    let _held = accept_subscription_stream(&broker.conn, 0x00).await;
    task.await.unwrap().unwrap();

    let (blocked, _blocked_rx) = start_subscribe(&client, "limit/2", QoS::AtMostOnce);
    let outcome = tokio::time::timeout(Duration::from_secs(12), blocked)
        .await
        .expect("SUBSCRIBE still waiting for a stream after the SUBACK limit")
        .unwrap();
    assert!(matches!(outcome, Err(MqttError::Timeout)), "{outcome:?}");
    assert!(client.is_connected().await);
}

#[tokio::test]
async fn disconnect_is_not_blocked_by_a_subscribe_waiting_for_a_stream() {
    let options = ConnectOptions::new("sub-streams-no-stream-disc").with_automatic_reconnect(false);
    let (client, broker) =
        connect_fake_broker_limited(options, true, Some(bidi_stream_limit(2))).await;
    let (task, _rx) = start_subscribe(&client, "limit/1", QoS::AtMostOnce);
    let _held = accept_subscription_stream(&broker.conn, 0x00).await;
    task.await.unwrap().unwrap();

    let (blocked, _blocked_rx) = start_subscribe(&client, "limit/2", QoS::AtMostOnce);
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(3), client.disconnect())
        .await
        .expect("disconnect blocked by a SUBSCRIBE waiting for a stream")
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(1), blocked)
        .await
        .expect("SUBSCRIBE kept waiting after disconnect")
        .unwrap();
    assert!(
        matches!(outcome, Err(MqttError::NotConnected)),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn stream_finished_right_after_suback_is_not_reused() {
    let (client, broker) = connect_fake_broker("sub-streams-fin", true).await;
    let (task, _rx) = start_subscribe(&client, "fin/a", QoS::AtMostOnce);
    let (mut send, mut recv) = broker.conn.accept_bi().await.unwrap();
    let subscribe = read_frame(&mut recv).await.unwrap();
    send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    send.finish().unwrap();
    task.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (task, _rx) = start_subscribe(&client, "fin/a", QoS::AtMostOnce);
    let _new_stream = tokio::time::timeout(
        Duration::from_secs(2),
        accept_subscription_stream(&broker.conn, 0x00),
    )
    .await
    .expect("SUBSCRIBE did not open a new stream");
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unsubscribe_racing_a_subscribe_of_the_same_filter_keeps_the_stream() {
    let (client, broker) = connect_fake_broker("sub-streams-unsub-race", true).await;
    let (task, _first_rx) = start_subscribe(&client, "race/a", QoS::AtLeastOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x01).await;
    task.await.unwrap().unwrap();

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("race/a").await });
    let unsubscribe_frame = read_frame(&mut recv).await.unwrap();
    assert_eq!(unsubscribe_frame[0], UNSUBSCRIBE);
    let (task, mut rx) = start_subscribe(&client, "race/a", QoS::AtLeastOnce);
    let subscribe_frame = read_frame(&mut recv).await.unwrap();
    assert_eq!(subscribe_frame[0], SUBSCRIBE);

    send.write_all(&[
        0xB0,
        0x04,
        unsubscribe_frame[2],
        unsubscribe_frame[3],
        0x00,
        0x00,
    ])
    .await
    .unwrap();
    unsubscribe.await.unwrap().unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    send.write_all(&[
        0x90,
        0x04,
        subscribe_frame[2],
        subscribe_frame[3],
        0x00,
        0x01,
    ])
    .await
    .unwrap();
    task.await.unwrap().unwrap();

    send.write_all(&publish_frame(1, "race/a", Some(5), b"later"))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("delivery did not reach the callback")
        .unwrap();
    assert_eq!(message.payload, b"later");
    let puback = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .unwrap()
        .expect("stream finished instead of carrying the PUBACK");
    assert_eq!(puback[0], PUBACK);
    assert_eq!(&puback[2..4], &5u16.to_be_bytes());
}

#[tokio::test]
async fn suback_with_reason_string_closes_the_connection_when_problem_information_is_off() {
    for subscription_streams in [false, true] {
        let mut options = ConnectOptions::new(format!("sub-streams-rpi-{subscription_streams}"))
            .with_automatic_reconnect(false);
        options.properties.request_problem_information = Some(false);
        let (client, mut broker) = connect_fake_broker_with(options, subscription_streams).await;
        let (task, _rx) = start_subscribe(&client, "rpi/a", QoS::AtMostOnce);
        let mut data_stream = if subscription_streams {
            Some(broker.conn.accept_bi().await.unwrap())
        } else {
            None
        };
        let (send, recv) = match data_stream.as_mut() {
            Some((send, recv)) => (send, recv),
            None => (&mut broker.ctl_send, &mut broker.ctl_recv),
        };
        let subscribe = read_frame(recv).await.unwrap();
        send.write_all(&[
            0x90,
            0x08,
            subscribe[2],
            subscribe[3],
            0x04,
            0x1F,
            0x00,
            0x01,
            b'x',
            0x00,
        ])
        .await
        .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            outcome.is_err(),
            "subscription_streams={subscription_streams}: SUBACK accepted"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !client.is_connected().await,
            "subscription_streams={subscription_streams}: connection still open"
        );
    }
}

#[tokio::test]
async fn publish_on_a_server_uni_stream_without_flow_header_reports_the_stream_id() {
    let (client, mut broker) = connect_fake_broker("sub-streams-uni", false).await;
    let (task, mut rx) = start_subscribe(&client, "plain/u", QoS::AtMostOnce);
    let subscribe = read_frame(&mut broker.ctl_recv).await.unwrap();
    broker
        .ctl_send
        .write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();

    let mut send = broker.conn.open_uni().await.unwrap();
    let server_stream_id = u64::from(send.id());
    send.write_all(&publish_frame(0, "plain/u", None, b"uni"))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.payload, b"uni");
    assert_eq!(message.stream_id, Some(server_stream_id));
}

fn deferred_ack_options(client_id: &str) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_automatic_reconnect(false)
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_session_expiry_interval(60)
        .with_receive_maximum(16)
}

#[tokio::test]
async fn deferred_ack_mode_acknowledges_plain_subscription_on_its_stream() {
    let options = deferred_ack_options("sub-streams-deferred-plain");
    let (client, mut broker) = connect_fake_broker_with(options, true).await;
    let (task, mut rx) = start_subscribe(&client, "deferred/a", QoS::AtLeastOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x01).await;
    task.await.unwrap().unwrap();

    send.write_all(&publish_frame(1, "deferred/a", Some(7), b"x"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let on_control = nothing_on(&mut broker.ctl_recv).await;
    assert!(on_control, "PUBACK went to the control stream");
    let puback = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .expect("no PUBACK on the subscription stream")
        .unwrap();
    assert_eq!(puback[0], PUBACK);
    assert_eq!(&puback[2..4], &7u16.to_be_bytes());
}

#[tokio::test]
async fn resolved_ack_token_acknowledges_on_the_subscription_stream() {
    let options = deferred_ack_options("sub-streams-deferred-token");
    let (client, mut broker) = connect_fake_broker_with(options, true).await;
    let (tx, mut tokens) = mpsc::unbounded_channel();
    let subscriber = client.clone();
    let task = tokio::spawn(async move {
        subscriber
            .subscribe_with_ack(
                "deferred/t",
                SubscribeOptions {
                    qos: QoS::AtLeastOnce,
                    ..Default::default()
                },
                move |_message, token| {
                    let _ = tx.send(token);
                },
            )
            .await
    });
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x01).await;
    task.await.unwrap().unwrap();

    send.write_all(&publish_frame(1, "deferred/t", Some(8), b"x"))
        .await
        .unwrap();
    let token = tokio::time::timeout(Duration::from_secs(2), tokens.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(nothing_on(&mut recv).await, "PUBACK before the token");
    token.ack();
    let on_control = nothing_on(&mut broker.ctl_recv).await;
    assert!(on_control, "token PUBACK went to the control stream");
    let puback = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .expect("no PUBACK on the subscription stream")
        .unwrap();
    assert_eq!(puback[0], PUBACK);
    assert_eq!(&puback[2..4], &8u16.to_be_bytes());
}

#[tokio::test]
async fn ack_token_uses_the_control_stream_when_its_stream_cannot_be_written() {
    let options = deferred_ack_options("sub-streams-deferred-finished");
    let (client, mut broker) = connect_fake_broker_with(options, true).await;
    let (tx, mut tokens) = mpsc::unbounded_channel();
    let subscriber = client.clone();
    let task = tokio::spawn(async move {
        subscriber
            .subscribe_with_ack(
                "deferred/f",
                SubscribeOptions {
                    qos: QoS::AtLeastOnce,
                    ..Default::default()
                },
                move |_message, token| {
                    let _ = tx.send(token);
                },
            )
            .await
    });
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x01).await;
    task.await.unwrap().unwrap();

    send.write_all(&publish_frame(1, "deferred/f", Some(9), b"x"))
        .await
        .unwrap();
    let token = tokio::time::timeout(Duration::from_secs(2), tokens.recv())
        .await
        .unwrap()
        .unwrap();

    recv.stop(0u32.into()).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    token.ack();
    let puback = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut broker.ctl_recv))
        .await
        .expect("no PUBACK on the control stream")
        .unwrap();
    assert_eq!(puback[0], PUBACK);
    assert_eq!(&puback[2..4], &9u16.to_be_bytes());
}

#[tokio::test]
async fn qos2_delivery_completes_on_the_subscription_stream() {
    let (client, mut broker) = connect_fake_broker("sub-streams-qos2", true).await;
    let (task, mut rx) = start_subscribe(&client, "two/a", QoS::ExactlyOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x02).await;
    task.await.unwrap().unwrap();

    send.write_all(&publish_frame(2, "two/a", Some(11), b"x"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let pubrec = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pubrec[0], PUBREC);
    send.write_all(&[PUBREL, 0x02, 0x00, 11]).await.unwrap();
    let pubcomp = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pubcomp[0], PUBCOMP);
    assert!(nothing_on(&mut broker.ctl_recv).await);
}

#[tokio::test]
async fn pubrel_after_unsuback_gets_its_pubcomp_on_the_stream() {
    let (client, mut broker) = connect_fake_broker("sub-streams-pubrel-late", true).await;
    let (task, mut rx) = start_subscribe(&client, "late/a", QoS::ExactlyOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x02).await;
    task.await.unwrap().unwrap();

    send.write_all(&publish_frame(2, "late/a", Some(11), b"x"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_frame(&mut recv).await.unwrap()[0], PUBREC);

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("late/a").await });
    let frame = read_frame(&mut recv).await.unwrap();
    assert_eq!(frame[0], UNSUBSCRIBE);
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    unsubscribe.await.unwrap().unwrap();

    send.write_all(&[PUBREL, 0x02, 0x00, 11]).await.unwrap();
    let pubcomp = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .expect("no PUBCOMP on the subscription stream")
        .expect("subscription stream finished before the PUBCOMP");
    assert_eq!(pubcomp[0], PUBCOMP);
    assert_eq!(&pubcomp[2..4], &11u16.to_be_bytes());
    assert!(nothing_on(&mut broker.ctl_recv).await);
}

#[tokio::test]
async fn cancelled_subscribe_leaves_the_filter_usable() {
    let (client, broker) = connect_fake_broker("sub-streams-cancelled", true).await;
    let (task, _rx) = start_subscribe(&client, "cancel/a", QoS::AtMostOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x00).await;
    task.await.unwrap().unwrap();

    let cancelled = tokio::time::timeout(
        Duration::from_millis(300),
        client.subscribe("cancel/a", |_message| {}),
    )
    .await;
    assert!(cancelled.is_err());
    assert_eq!(read_frame(&mut recv).await.unwrap()[0], SUBSCRIBE);

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("cancel/a").await });
    let frame = read_frame(&mut recv).await.unwrap();
    assert_eq!(frame[0], UNSUBSCRIBE);
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    unsubscribe.await.unwrap().unwrap();
    assert!(
        nothing_on(&mut recv).await,
        "subscription stream must stay open after the unsubscribe"
    );

    let (task, mut rx) = start_subscribe(&client, "cancel/a", QoS::AtLeastOnce);
    let subscribe = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .expect("resubscribe did not reuse the idle stream")
        .unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x01])
        .await
        .unwrap();
    task.await.unwrap().unwrap();
    let (mut new_send, mut new_recv) = (send, recv);
    new_send
        .write_all(&publish_frame(1, "cancel/a", Some(4), b"again"))
        .await
        .unwrap();
    let message = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.payload, b"again");
    let puback = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut new_recv))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(puback[0], PUBACK);
    assert_eq!(&puback[2..4], &4u16.to_be_bytes());
}

#[tokio::test]
async fn unsubscribe_while_the_first_subscribe_awaits_its_suback_uses_the_stream() {
    let (client, mut broker) = connect_fake_broker("sub-streams-unsub-pending", true).await;
    let (task, _rx) = start_subscribe(&client, "pend/a", QoS::AtMostOnce);
    let (mut send, mut recv) = broker.conn.accept_bi().await.unwrap();
    let subscribe = read_frame(&mut recv).await.unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("pend/a").await });
    let frame = tokio::time::timeout(Duration::from_secs(1), read_frame(&mut recv))
        .await
        .expect("UNSUBSCRIBE not sent on the filter's stream")
        .unwrap();
    assert_eq!(frame[0], UNSUBSCRIBE);
    assert!(nothing_on(&mut broker.ctl_recv).await);

    send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();
    unsubscribe.await.unwrap().unwrap();
    assert!(nothing_on(&mut recv).await);

    let (task, _rx) = start_subscribe(&client, "pend/a", QoS::AtMostOnce);
    let again = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .expect("resubscribe did not reuse the idle stream")
        .unwrap();
    assert_eq!(again[0], SUBSCRIBE);
    send.write_all(&[0x90, 0x04, again[2], again[3], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn stream_the_broker_stopped_reading_is_replaced_after_a_failed_write() {
    let (client, broker) = connect_fake_broker("sub-streams-stop-sending", true).await;
    let (task, _rx) = start_subscribe(&client, "stop/a", QoS::AtMostOnce);
    let (_send, mut recv) = accept_subscription_stream(&broker.conn, 0x00).await;
    task.await.unwrap().unwrap();

    recv.stop(0u32.into()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(client.is_connected().await);

    let (task, _rx) = start_subscribe(&client, "stop/a", QoS::AtMostOnce);
    let failed = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .expect("SUBSCRIBE on the stopped stream did not fail")
        .unwrap();
    assert!(failed.is_err(), "{failed:?}");

    let (task, _rx) = start_subscribe(&client, "stop/a", QoS::AtMostOnce);
    let (mut send, mut new_recv) = tokio::time::timeout(
        Duration::from_secs(2),
        accept_subscription_stream(&broker.conn, 0x00),
    )
    .await
    .expect("SUBSCRIBE after the failed write did not open a new stream");
    task.await.unwrap().unwrap();

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("stop/a").await });
    let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut new_recv))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(frame[0], UNSUBSCRIBE);
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    unsubscribe.await.unwrap().unwrap();
}

#[tokio::test]
async fn unsuback_keeps_the_mapping_of_a_subscribe_sent_after_the_unsubscribe() {
    let (client, mut broker) = connect_fake_broker("sub-streams-unsuback-newer", true).await;
    let (task, _first_rx) = start_subscribe(&client, "newer/a", QoS::AtLeastOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x01).await;
    task.await.unwrap().unwrap();

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("newer/a").await });
    let unsubscribe_frame = read_frame(&mut recv).await.unwrap();
    assert_eq!(unsubscribe_frame[0], UNSUBSCRIBE);
    let (task, _rx) = start_subscribe(&client, "newer/a", QoS::AtLeastOnce);
    let subscribe_frame = read_frame(&mut recv).await.unwrap();
    assert_eq!(subscribe_frame[0], SUBSCRIBE);
    send.write_all(&[
        0xB0,
        0x04,
        unsubscribe_frame[2],
        unsubscribe_frame[3],
        0x00,
        0x00,
    ])
    .await
    .unwrap();
    unsubscribe.await.unwrap().unwrap();
    send.write_all(&[
        0x90,
        0x04,
        subscribe_frame[2],
        subscribe_frame[3],
        0x00,
        0x01,
    ])
    .await
    .unwrap();
    task.await.unwrap().unwrap();

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("newer/a").await });
    let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .expect("second UNSUBSCRIBE did not go on the subscription stream")
        .unwrap();
    assert_eq!(frame[0], UNSUBSCRIBE);
    assert!(nothing_on(&mut broker.ctl_recv).await);
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    unsubscribe.await.unwrap().unwrap();
}

#[tokio::test]
async fn control_stream_unsuback_keeps_the_mapping_of_a_later_subscribe() {
    let (client, mut broker) = connect_fake_broker("sub-streams-unsuback-control", true).await;

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("ctl/a").await });
    let unsubscribe_frame = read_frame(&mut broker.ctl_recv).await.unwrap();
    assert_eq!(unsubscribe_frame[0], UNSUBSCRIBE);

    let (task, _rx) = start_subscribe(&client, "ctl/a", QoS::AtMostOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x00).await;
    task.await.unwrap().unwrap();
    broker
        .ctl_send
        .write_all(&[
            0xB0,
            0x04,
            unsubscribe_frame[2],
            unsubscribe_frame[3],
            0x00,
            0x00,
        ])
        .await
        .unwrap();
    unsubscribe.await.unwrap().unwrap();

    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("ctl/a").await });
    let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .expect("UNSUBSCRIBE did not go on the subscription stream")
        .unwrap();
    assert_eq!(frame[0], UNSUBSCRIBE);
    assert!(nothing_on(&mut broker.ctl_recv).await);
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    unsubscribe.await.unwrap().unwrap();
}

async fn read_header(recv: &mut quinn::RecvStream) -> (u8, usize, [u8; 2]) {
    let first = recv.read_u8().await.unwrap();
    let mut remaining = 0usize;
    let mut multiplier = 1usize;
    loop {
        let byte = recv.read_u8().await.unwrap();
        remaining += usize::from(byte & 0x7F) * multiplier;
        multiplier *= 128;
        if byte & 0x80 == 0 {
            break;
        }
    }
    let mut id = [0u8; 2];
    recv.read_exact(&mut id).await.unwrap();
    (first, remaining - 2, id)
}

async fn subscribe_queued_behind_an_unsubscribe(stream_receive_window: u32) {
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(stream_receive_window.into());
    let options = ConnectOptions::new("queued-behind-unsubscribe").with_automatic_reconnect(false);
    let (client, mut broker) = connect_fake_broker_limited(options, true, Some(transport)).await;
    let filter = format!("q/{}", "x".repeat(3000));

    let (task, _rx) = start_subscribe(&client, &filter, QoS::AtMostOnce);
    let (mut send, mut recv) = broker.conn.accept_bi().await.unwrap();
    let subscribe = read_frame(&mut recv).await.unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    send.write_all(&[0x90, 0x04, subscribe[3], subscribe[4], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();

    let unsubscriber = client.clone();
    let unsubscribe_filter = filter.clone();
    let unsubscribe =
        tokio::spawn(async move { unsubscriber.unsubscribe(unsubscribe_filter).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (second, _second_rx) = start_subscribe(&client, &filter, QoS::AtMostOnce);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (first_byte, rest, unsubscribe_id) = read_header(&mut recv).await;
    assert_eq!(first_byte, UNSUBSCRIBE);
    send.write_all(&[0xB0, 0x04, unsubscribe_id[0], unsubscribe_id[1], 0x00, 0x00])
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut body = vec![0u8; rest];
    recv.read_exact(&mut body).await.unwrap();
    unsubscribe.await.unwrap().unwrap();

    let resubscribe = read_frame(&mut recv).await.unwrap();
    assert_eq!(resubscribe[0], SUBSCRIBE);
    send.write_all(&[0x90, 0x04, resubscribe[3], resubscribe[4], 0x00, 0x00])
        .await
        .unwrap();
    second.await.unwrap().unwrap();

    let unsubscriber = client.clone();
    let unsubscribe_filter = filter.clone();
    let unsubscribe =
        tokio::spawn(async move { unsubscriber.unsubscribe(unsubscribe_filter).await });
    tokio::select! {
        frame = read_frame(&mut recv) => assert_eq!(frame.unwrap()[0], UNSUBSCRIBE),
        frame = read_frame(&mut broker.ctl_recv) => panic!(
            "UNSUBSCRIBE of a filter whose SUBSCRIBE was sent on its stream went to the control stream: {:#04x}",
            frame.unwrap()[0]
        ),
        () = tokio::time::sleep(Duration::from_secs(3)) => panic!("no UNSUBSCRIBE was sent"),
    }
    unsubscribe.abort();
}

#[tokio::test]
async fn subscribe_queued_behind_a_blocked_unsubscribe_keeps_its_mapping() {
    subscribe_queued_behind_an_unsubscribe(1000).await;
}

#[tokio::test]
async fn subscribe_after_an_unblocked_unsubscribe_keeps_its_mapping() {
    subscribe_queued_behind_an_unsubscribe(1_000_000).await;
}

#[tokio::test]
async fn subscribe_queued_while_its_stream_ends_opens_a_new_stream() {
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(1000u32.into());
    let options = ConnectOptions::new("queued-while-stream-ends").with_automatic_reconnect(false);
    let (client, mut broker) = connect_fake_broker_limited(options, true, Some(transport)).await;
    let filter = format!("q/{}", "x".repeat(3000));

    let (task, _rx) = start_subscribe(&client, &filter, QoS::AtMostOnce);
    let (mut send, mut recv) = broker.conn.accept_bi().await.unwrap();
    let subscribe = read_frame(&mut recv).await.unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    send.write_all(&[0x90, 0x04, subscribe[3], subscribe[4], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();

    let (blocked, _blocked_rx) = start_subscribe(&client, &filter, QoS::AtMostOnce);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (queued, _queued_rx) = start_subscribe(&client, &filter, QoS::AtMostOnce);
    tokio::time::sleep(Duration::from_millis(200)).await;

    send.finish().unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let frame = read_frame(&mut recv).await.unwrap();
    assert_eq!(frame[0], SUBSCRIBE);
    broker
        .ctl_send
        .write_all(&[0x90, 0x04, frame[3], frame[4], 0x00, 0x00])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), blocked)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (mut new_send, mut new_recv) =
        tokio::time::timeout(Duration::from_secs(3), broker.conn.accept_bi())
            .await
            .expect("the SUBSCRIBE queued behind the ended stream did not open a new stream")
            .unwrap();
    let frame = read_frame(&mut new_recv).await.unwrap();
    assert_eq!(frame[0], SUBSCRIBE);
    new_send
        .write_all(&[0x90, 0x04, frame[3], frame[4], 0x00, 0x00])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let (third, _third_rx) = start_subscribe(&client, &filter, QoS::AtMostOnce);
    let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut new_recv))
        .await
        .expect("the next SUBSCRIBE did not use the filter's new stream")
        .unwrap();
    assert_eq!(frame[0], SUBSCRIBE);
    new_send
        .write_all(&[0x90, 0x04, frame[3], frame[4], 0x00, 0x00])
        .await
        .unwrap();
    third.await.unwrap().unwrap();
    let on_old = tokio::time::timeout(Duration::from_millis(300), read_frame(&mut recv)).await;
    assert!(
        !matches!(on_old, Ok(Some(_))),
        "a packet went on the stream whose reader exited: {on_old:?}"
    );
}

type BrokerStream = (quinn::SendStream, quinn::RecvStream);

enum NextFrame {
    Opened(BrokerStream),
    OnLatest(Option<Vec<u8>>),
}

async fn next_subscription_frame(
    conn: &quinn::Connection,
    held: &mut Vec<BrokerStream>,
) -> Vec<u8> {
    let next = {
        let latest = async {
            match held.last_mut() {
                Some((_, recv)) => read_frame(recv).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            accepted = conn.accept_bi() => NextFrame::Opened(accepted.unwrap()),
            frame = latest => NextFrame::OnLatest(frame),
            () = tokio::time::sleep(Duration::from_secs(12)) => panic!("no SUBSCRIBE on any stream"),
        }
    };
    match next {
        NextFrame::Opened(stream) => {
            held.push(stream);
            let (_, recv) = held.last_mut().unwrap();
            read_frame(recv).await.unwrap()
        }
        NextFrame::OnLatest(frame) => frame.unwrap(),
    }
}

async fn churn_cycle(
    client: &MqttClient,
    conn: &quinn::Connection,
    held: &mut Vec<BrokerStream>,
    filter: &str,
) {
    let (task, _rx) = start_subscribe(client, filter, QoS::AtMostOnce);
    let subscribe = next_subscription_frame(conn, held).await;
    assert_eq!(subscribe[0], SUBSCRIBE);
    let (send, recv) = held.last_mut().unwrap();
    send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(12), task)
        .await
        .expect("SUBSCRIBE did not complete")
        .unwrap()
        .unwrap();
    let unsubscriber = client.clone();
    let owned = filter.to_string();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe(owned).await });
    let frame = read_frame(recv).await.unwrap();
    assert_eq!(frame[0], UNSUBSCRIBE);
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    unsubscribe.await.unwrap().unwrap();
}

#[tokio::test]
async fn resubscribing_one_filter_reuses_its_idle_stream() {
    let options = ConnectOptions::new("sub-streams-churn").with_automatic_reconnect(false);
    let (client, broker) =
        connect_fake_broker_limited(options, true, Some(bidi_stream_limit(3))).await;
    let mut held = Vec::new();
    churn_cycle(&client, &broker.conn, &mut held, "churn/a").await;
    churn_cycle(&client, &broker.conn, &mut held, "churn/a").await;
    let (task, _rx) = start_subscribe(&client, "churn/a", QoS::AtMostOnce);
    let subscribe = next_subscription_frame(&broker.conn, &mut held).await;
    let (send, _) = held.last_mut().unwrap();
    send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(12), task)
        .await
        .unwrap()
        .unwrap();
    assert!(outcome.is_ok(), "{outcome:?}");
    assert_eq!(held.len(), 1, "idle stream was not reused");
}

#[tokio::test]
async fn fifty_subscribe_cycles_of_one_filter_fit_a_small_stream_limit() {
    let options = ConnectOptions::new("sub-streams-churn-50").with_automatic_reconnect(false);
    let (client, broker) =
        connect_fake_broker_limited(options, true, Some(bidi_stream_limit(3))).await;
    let mut held = Vec::new();
    for _ in 0..50 {
        churn_cycle(&client, &broker.conn, &mut held, "churn/b").await;
    }
    assert_eq!(held.len(), 1);
}

#[tokio::test]
async fn disconnect_interrupts_a_stream_open_without_waiting_for_the_client_lock() {
    let options =
        ConnectOptions::new("sub-streams-interrupt-queued").with_automatic_reconnect(false);
    let (client, broker) =
        connect_fake_broker_limited(options, true, Some(bidi_stream_limit(2))).await;
    let (task, _rx) = start_subscribe(&client, "limit/1", QoS::AtMostOnce);
    let _held = accept_subscription_stream(&broker.conn, 0x00).await;
    task.await.unwrap().unwrap();

    let (blocked, _blocked_rx) = start_subscribe(&client, "limit/2", QoS::AtMostOnce);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let setter = client.clone();
    let queued_writer = tokio::spawn(async move { setter.set_queue_on_disconnect(true).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(15), client.disconnect())
        .await
        .unwrap()
        .unwrap();
    let elapsed = started.elapsed();
    let outcome = blocked.await.unwrap();
    queued_writer.await.unwrap();
    assert!(
        elapsed < Duration::from_secs(3),
        "disconnect waited {elapsed:?}"
    );
    assert!(
        matches!(outcome, Err(MqttError::NotConnected)),
        "{outcome:?}"
    );
}

#[tokio::test]
async fn delivery_on_a_stream_the_broker_stopped_reading_is_acked_on_the_control_stream() {
    let (client, mut broker) = connect_fake_broker("sub-streams-stopped-ack", true).await;
    let (task, mut rx) = start_subscribe(&client, "stopped/a", QoS::AtLeastOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x01).await;
    task.await.unwrap().unwrap();

    recv.stop(0u32.into()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    send.write_all(&publish_frame(1, "stopped/a", Some(9), b"first"))
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("QoS 1 delivery on the stopped stream was dropped")
        .unwrap();
    assert_eq!(first.payload, b"first");
    let puback = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut broker.ctl_recv))
        .await
        .expect("no PUBACK on the control stream")
        .unwrap();
    assert_eq!(puback[0], PUBACK);
    assert_eq!(&puback[2..4], &9u16.to_be_bytes());

    send.write_all(&publish_frame(0, "stopped/a", None, b"second"))
        .await
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("later delivery on the stream was not read")
        .unwrap();
    assert_eq!(second.payload, b"second");
    assert!(client.is_connected().await);

    let (task, _rx) = start_subscribe(&client, "stopped/a", QoS::AtMostOnce);
    let _other = tokio::time::timeout(
        Duration::from_secs(2),
        accept_subscription_stream(&broker.conn, 0x00),
    )
    .await
    .expect("SUBSCRIBE did not open a new stream");
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn qos2_delivery_on_a_stopped_stream_completes_on_the_control_stream() {
    let (client, mut broker) = connect_fake_broker("sub-streams-stopped-qos2", true).await;
    let (task, mut rx) = start_subscribe(&client, "stopped/q2", QoS::ExactlyOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x02).await;
    task.await.unwrap().unwrap();

    recv.stop(0u32.into()).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    send.write_all(&publish_frame(2, "stopped/q2", Some(12), b"x"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("QoS 2 delivery on the stopped stream was dropped")
        .unwrap();
    let pubrec = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut broker.ctl_recv))
        .await
        .expect("no PUBREC on the control stream")
        .unwrap();
    assert_eq!(pubrec[0], PUBREC);
    send.write_all(&[PUBREL, 0x02, 0x00, 12]).await.unwrap();
    let pubcomp = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut broker.ctl_recv))
        .await
        .expect("no PUBCOMP on the control stream")
        .unwrap();
    assert_eq!(pubcomp[0], PUBCOMP);
}

async fn two_subscribes_after_idle(publishes: u16) -> (usize, usize) {
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(1000u32.into());
    let options = ConnectOptions::new("sub-streams-idle-shared").with_automatic_reconnect(false);
    let (client, broker) = connect_fake_broker_limited(options, true, Some(transport)).await;

    let (task, _rx) = start_subscribe(&client, "idle/x", QoS::AtLeastOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x01).await;
    task.await.unwrap().unwrap();
    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("idle/x").await });
    let frame = read_frame(&mut recv).await.unwrap();
    assert_eq!(frame[0], UNSUBSCRIBE);
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    unsubscribe.await.unwrap().unwrap();

    for id in 1..=publishes {
        send.write_all(&publish_frame(1, "idle/x", Some(id), b"p"))
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let (first, _first_rx) = start_subscribe(&client, "idle/y", QoS::AtMostOnce);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (second, _second_rx) = start_subscribe(&client, "idle/z", QoS::AtMostOnce);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut subscribes_on_idle = 0;
    while let Ok(Some(frame)) =
        tokio::time::timeout(Duration::from_millis(700), read_frame(&mut recv)).await
    {
        match frame[0] {
            SUBSCRIBE => {
                subscribes_on_idle += 1;
                send.write_all(&[0x90, 0x04, frame[2], frame[3], 0x00, 0x00])
                    .await
                    .unwrap();
            }
            PUBACK => {}
            other => panic!("unexpected {other:#04x}"),
        }
    }
    let mut new_streams = 0;
    while let Ok(Ok((mut new_send, mut new_recv))) =
        tokio::time::timeout(Duration::from_millis(700), broker.conn.accept_bi()).await
    {
        new_streams += 1;
        let frame = read_frame(&mut new_recv).await.unwrap();
        new_send
            .write_all(&[0x90, 0x04, frame[2], frame[3], 0x00, 0x00])
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    (subscribes_on_idle, new_streams)
}

#[tokio::test]
async fn concurrent_subscribes_of_two_filters_never_share_an_idle_stream() {
    let (subscribes_on_idle, new_streams) = two_subscribes_after_idle(400).await;
    assert_eq!(
        (subscribes_on_idle, new_streams),
        (1, 1),
        "two concurrent SUBSCRIBEs of different filters shared the idle stream"
    );
}

#[tokio::test]
async fn concurrent_subscribes_of_two_filters_with_an_idle_writer_take_one_stream_each() {
    let (subscribes_on_idle, new_streams) = two_subscribes_after_idle(0).await;
    assert_eq!((subscribes_on_idle, new_streams), (1, 1));
}

#[tokio::test]
async fn feature_off_suback_on_a_server_opened_stream_is_ignored() {
    let (client, mut broker) = connect_fake_broker("sub-streams-server-suback", false).await;
    let (task, _rx) = start_subscribe(&client, "plain/srv", QoS::AtMostOnce);
    let subscribe = read_frame(&mut broker.ctl_recv).await.unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    let (mut send, _recv) = broker.conn.open_bi().await.unwrap();
    send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    let mut task = task;
    let completed = tokio::time::timeout(Duration::from_secs(2), &mut task).await;
    assert!(
        completed.is_err(),
        "SUBACK on a server-opened stream completed a control-stream SUBSCRIBE"
    );
    broker
        .ctl_send
        .write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn token_ack_fallback_stops_using_the_stopped_stream() {
    let options = deferred_ack_options("sub-streams-token-fallback");
    let (client, mut broker) = connect_fake_broker_with(options, true).await;
    let (tx, mut tokens) = mpsc::unbounded_channel();
    let subscriber = client.clone();
    let task = tokio::spawn(async move {
        subscriber
            .subscribe_with_ack(
                "deferred/m",
                SubscribeOptions {
                    qos: QoS::AtLeastOnce,
                    ..Default::default()
                },
                move |_message, token| {
                    let _ = tx.send(token);
                },
            )
            .await
    });
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x01).await;
    task.await.unwrap().unwrap();
    send.write_all(&publish_frame(1, "deferred/m", Some(9), b"x"))
        .await
        .unwrap();
    let token = tokio::time::timeout(Duration::from_secs(2), tokens.recv())
        .await
        .unwrap()
        .unwrap();
    recv.stop(0u32.into()).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    token.ack();
    let puback = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut broker.ctl_recv))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(puback[0], PUBACK);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (again, _rx) = start_subscribe(&client, "deferred/m", QoS::AtLeastOnce);
    let _other = tokio::time::timeout(
        Duration::from_secs(2),
        accept_subscription_stream(&broker.conn, 0x01),
    )
    .await
    .expect("SUBSCRIBE after a failed token ack went to the stopped stream");
    tokio::time::timeout(Duration::from_secs(2), again)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn disconnect_during_the_connack_wait_stays_in_force() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs");
    let certs = QuicAcceptorConfig::load_cert_chain_from_file(cert_dir.join("server.pem"))
        .await
        .unwrap();
    let key = QuicAcceptorConfig::load_private_key_from_file(cert_dir.join("server.key"))
        .await
        .unwrap();
    let mut server_config = QuicAcceptorConfig::new(certs, key)
        .with_alpn_protocols(vec![b"mqtt".to_vec()])
        .build_server_config()
        .unwrap();
    server_config.transport_config(std::sync::Arc::new(bidi_stream_limit(1)));
    let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let url = format!("quic://{}", endpoint.local_addr().unwrap());

    let options =
        ConnectOptions::new("sub-streams-interrupt-connack").with_automatic_reconnect(false);
    let client = MqttClient::with_options(options.clone());
    client.set_insecure_tls(true).await;
    client.set_quic_subscription_streams(true).await;
    let connecting = client.clone();
    let connect =
        tokio::spawn(async move { Box::pin(connecting.connect_with_options(&url, options)).await });

    let conn = tokio::time::timeout(Duration::from_secs(5), async {
        endpoint.accept().await.unwrap().await.unwrap()
    })
    .await
    .unwrap();
    let (mut ctl_send, mut ctl_recv) = conn.accept_bi().await.unwrap();
    let connect_frame = read_frame(&mut ctl_recv).await.unwrap();
    assert_eq!(connect_frame[0] >> 4, 1);

    let (blocked, _rx) = start_subscribe(&client, "limit/a", QoS::AtMostOnce);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let disconnecting = client.clone();
    let disconnect = tokio::spawn(async move {
        let started = std::time::Instant::now();
        let outcome = disconnecting.disconnect().await;
        (started.elapsed(), outcome)
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    ctl_send
        .write_all(&[0x20, 0x03, 0x00, 0x00, 0x00])
        .await
        .unwrap();
    connect.await.unwrap().unwrap();

    let (elapsed, outcome) = tokio::time::timeout(Duration::from_secs(15), disconnect)
        .await
        .unwrap()
        .unwrap();
    outcome.unwrap();
    let subscribed = tokio::time::timeout(Duration::from_secs(2), blocked)
        .await
        .unwrap()
        .unwrap();
    assert!(
        elapsed < Duration::from_secs(3),
        "disconnect waited {elapsed:?} for a SUBSCRIBE blocked on a stream open"
    );
    assert!(subscribed.is_err(), "{subscribed:?}");
}

#[tokio::test]
async fn refused_filters_do_not_hold_streams_under_a_stream_limit() {
    let options = ConnectOptions::new("sub-streams-refused-limit").with_automatic_reconnect(false);
    let (client, broker) =
        connect_fake_broker_limited(options, true, Some(bidi_stream_limit(3))).await;
    let mut held = Vec::new();
    for (filter, granted) in [("denied/1", 0x87), ("denied/2", 0x87), ("allowed/1", 0x00)] {
        let (task, _rx) = start_subscribe(&client, filter, QoS::AtMostOnce);
        let subscribe = next_subscription_frame(&broker.conn, &mut held).await;
        assert_eq!(subscribe[0], SUBSCRIBE);
        let (send, _) = held.last_mut().unwrap();
        send.write_all(&[0x90, 0x04, subscribe[2], subscribe[3], 0x00, granted])
            .await
            .unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(12), task)
            .await
            .unwrap()
            .unwrap();
        if granted == 0x00 {
            assert!(outcome.is_ok(), "{outcome:?}");
        } else {
            assert!(
                matches!(outcome, Err(MqttError::SubscriptionDenied(_))),
                "{outcome:?}"
            );
        }
    }
    assert_eq!(held.len(), 1, "a refused filter kept its stream");
}

#[tokio::test]
async fn subscribe_whose_suback_times_out_leaves_its_stream_idle() {
    let (client, broker) = connect_fake_broker("sub-streams-suback-timeout", true).await;
    let (task, _rx) = start_subscribe(&client, "silent/a", QoS::AtMostOnce);
    let (mut send, mut recv) = broker.conn.accept_bi().await.unwrap();
    let subscribe = read_frame(&mut recv).await.unwrap();
    assert_eq!(subscribe[0], SUBSCRIBE);
    let outcome = tokio::time::timeout(Duration::from_secs(12), task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(outcome, Err(MqttError::Timeout)), "{outcome:?}");

    let (task, _rx) = start_subscribe(&client, "silent/b", QoS::AtMostOnce);
    let frame = tokio::time::timeout(Duration::from_secs(2), read_frame(&mut recv))
        .await
        .expect("SUBSCRIBE did not reuse the stream of the timed-out SUBSCRIBE")
        .unwrap();
    assert_eq!(frame[0], SUBSCRIBE);
    send.write_all(&[0x90, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn unsubscribe_is_not_held_back_by_a_subscribe_waiting_for_a_stream() {
    let options =
        ConnectOptions::new("sub-streams-unsub-behind-open").with_automatic_reconnect(false);
    let (client, broker) =
        connect_fake_broker_limited(options, true, Some(bidi_stream_limit(2))).await;
    let (task, _rx) = start_subscribe(&client, "held/a", QoS::AtMostOnce);
    let (mut send, mut recv) = accept_subscription_stream(&broker.conn, 0x00).await;
    task.await.unwrap().unwrap();

    let (blocked, _blocked_rx) = start_subscribe(&client, "held/b", QoS::AtMostOnce);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = std::time::Instant::now();
    let unsubscriber = client.clone();
    let unsubscribe = tokio::spawn(async move { unsubscriber.unsubscribe("held/a").await });
    let frame = tokio::time::timeout(Duration::from_secs(12), read_frame(&mut recv))
        .await
        .unwrap()
        .unwrap();
    let waited = started.elapsed();
    assert_eq!(frame[0], UNSUBSCRIBE);
    assert!(
        waited < Duration::from_secs(2),
        "UNSUBSCRIBE held back {waited:?}"
    );
    send.write_all(&[0xB0, 0x04, frame[2], frame[3], 0x00, 0x00])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), unsubscribe)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    blocked.abort();
}
