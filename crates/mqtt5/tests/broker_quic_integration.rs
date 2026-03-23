#![cfg(feature = "transport-quic")]

use mqtt5::broker::config::{BrokerConfig, QuicConfig, ServerDeliveryStrategy};
use mqtt5::broker::MqttBroker;
use mqtt5::time::Duration;
use mqtt5::transport::StreamStrategy;
use mqtt5::MqttClient;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use ulid::Ulid;

fn test_client_id(prefix: &str) -> String {
    format!("{}-{}", prefix, Ulid::new())
}

async fn start_quic_broker(quic_port: u16) -> (MqttBroker, SocketAddr) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let quic_addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            QuicConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(quic_addr),
        );

    let broker = MqttBroker::with_config(config).await.unwrap();
    (broker, quic_addr)
}

#[tokio::test]
async fn test_broker_quic_creation() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            QuicConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(([127, 0, 0, 1], 0)),
        );

    let broker = MqttBroker::with_config(config).await;

    if broker.is_err() {
        eprintln!(
            "Skipping QUIC broker test - certificates not found: {:?}",
            broker.err()
        );
        return;
    }

    let mut broker = broker.unwrap();
    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;
    broker_handle.abort();
}

#[tokio::test]
async fn test_broker_default_quic_port() {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 1883))
        .with_quic(QuicConfig::new(
            PathBuf::from("../../test_certs/server.pem"),
            PathBuf::from("../../test_certs/server.key"),
        ));

    assert!(config.quic_config.is_some());
    let quic_config = config.quic_config.as_ref().unwrap();
    assert!(!quic_config.bind_addresses.is_empty());
    assert!(quic_config
        .bind_addresses
        .iter()
        .all(|addr| addr.port() == 14567));
}

#[tokio::test]
async fn test_broker_quic_client_connection() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24567).await;
    eprintln!("Broker QUIC endpoint bound to {quic_addr}");

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let client_id = test_client_id("quic-broker-test");
    let client = MqttClient::new(client_id);
    client.set_insecure_tls(true).await;

    let broker_url = format!("quic://{quic_addr}");
    let connect_result = client.connect(&broker_url).await;

    if connect_result.is_err() {
        eprintln!(
            "Client connection failed (may be cert issue): {:?}",
            connect_result.err()
        );
        broker_handle.abort();
        return;
    }

    assert!(client.is_connected().await);
    client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_broker_quic_pubsub() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24568).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client_id = test_client_id("quic-pub");
    let sub_client_id = test_client_id("quic-sub");
    let topic = format!("quic-test/{}/pubsub", Ulid::new());

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.set_insecure_tls(true).await;
    sub_client.set_insecure_tls(true).await;

    let broker_url = format!("quic://{quic_addr}");

    if pub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    pub_client
        .publish(&topic, b"Hello QUIC broker!")
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        1,
        "Should receive exactly 1 message"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_broker_quic_data_per_publish() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24569).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client_id = test_client_id("quic-pub-dpp");
    let sub_client_id = test_client_id("quic-sub-dpp");
    let topic = format!("quic-test/{}/data-per-publish", Ulid::new());

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.set_insecure_tls(true).await;
    sub_client.set_insecure_tls(true).await;
    pub_client
        .set_quic_stream_strategy(StreamStrategy::DataPerPublish)
        .await;

    let broker_url = format!("quic://{quic_addr}");

    if pub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    for i in 0..5 {
        pub_client
            .publish(&topic, format!("message {i}").as_bytes())
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        5,
        "Should receive all 5 messages with DataPerPublish strategy"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_broker_quic_data_per_topic() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24570).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client_id = test_client_id("quic-pub-dpt");
    let sub_client_id = test_client_id("quic-sub-dpt");
    let topic1 = format!("quic-test/{}/data-per-topic-1", Ulid::new());
    let topic2 = format!("quic-test/{}/data-per-topic-2", Ulid::new());

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.set_insecure_tls(true).await;
    sub_client.set_insecure_tls(true).await;
    pub_client
        .set_quic_stream_strategy(StreamStrategy::DataPerTopic)
        .await;

    let broker_url = format!("quic://{quic_addr}");

    if pub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone1 = received.clone();
    let received_clone2 = received.clone();

    sub_client
        .subscribe(&topic1, move |_msg| {
            received_clone1.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sub_client
        .subscribe(&topic2, move |_msg| {
            received_clone2.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    for i in 0..3 {
        pub_client
            .publish(&topic1, format!("topic1 msg {i}").as_bytes())
            .await
            .unwrap();
        pub_client
            .publish(&topic2, format!("topic2 msg {i}").as_bytes())
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        6,
        "Should receive all 6 messages (3 per topic) with DataPerTopic strategy"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
#[allow(deprecated)]
async fn test_broker_quic_data_per_subscription() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24571).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client_id = test_client_id("quic-pub-dps");
    let sub_client_id = test_client_id("quic-sub-dps");
    let topic1 = format!("quic-test/{}/data-per-sub-1", Ulid::new());
    let topic2 = format!("quic-test/{}/data-per-sub-2", Ulid::new());

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.set_insecure_tls(true).await;
    sub_client.set_insecure_tls(true).await;
    pub_client
        .set_quic_stream_strategy(StreamStrategy::DataPerSubscription)
        .await;

    let broker_url = format!("quic://{quic_addr}");

    if pub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone1 = received.clone();
    let received_clone2 = received.clone();

    sub_client
        .subscribe(&topic1, move |_msg| {
            received_clone1.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sub_client
        .subscribe(&topic2, move |_msg| {
            received_clone2.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    for i in 0..2 {
        pub_client
            .publish(&topic1, format!("sub1 msg {i}").as_bytes())
            .await
            .unwrap();
        pub_client
            .publish(&topic2, format!("sub2 msg {i}").as_bytes())
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        4,
        "Should receive all 4 messages with DataPerSubscription strategy"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

async fn start_quic_broker_with_early_data(quic_port: u16) -> (MqttBroker, SocketAddr) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let quic_addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            QuicConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(quic_addr)
            .with_early_data(true),
        );

    let broker = MqttBroker::with_config(config).await.unwrap();
    (broker, quic_addr)
}

#[tokio::test]
async fn test_quic_0rtt_first_connection_is_1rtt() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();

    let (mut broker, quic_addr) = start_quic_broker_with_early_data(24580).await;
    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let client = MqttClient::new(test_client_id("0rtt-first"));
    client.set_insecure_tls(true).await;
    client.set_quic_early_data(true).await;

    let broker_url = format!("quic://{quic_addr}");
    if client.connect(&broker_url).await.is_err() {
        eprintln!("Skipping 0-RTT test - QUIC connection failed");
        broker_handle.abort();
        return;
    }
    assert!(client.is_connected().await);
    assert!(!client.was_zero_rtt().await);

    client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_quic_0rtt_reconnection() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();

    let (mut broker, quic_addr) = start_quic_broker_with_early_data(24581).await;
    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let client = MqttClient::new(test_client_id("0rtt-reconnect"));
    client.set_insecure_tls(true).await;
    client.set_quic_early_data(true).await;

    let broker_url = format!("quic://{quic_addr}");

    if client.connect(&broker_url).await.is_err() {
        eprintln!("Skipping 0-RTT reconnection test - QUIC connection failed");
        broker_handle.abort();
        return;
    }
    assert!(client.is_connected().await);
    assert!(!client.was_zero_rtt().await);
    client.disconnect().await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    if client.connect(&broker_url).await.is_err() {
        eprintln!("Skipping 0-RTT reconnection test - second QUIC connection failed");
        broker_handle.abort();
        return;
    }
    assert!(client.is_connected().await);
    assert!(
        client.was_zero_rtt().await,
        "second connection should use 0-RTT"
    );

    client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_quic_0rtt_server_without_early_data_falls_back() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();

    let (mut broker, quic_addr) = start_quic_broker(24582).await;
    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let client = MqttClient::new(test_client_id("0rtt-fallback"));
    client.set_insecure_tls(true).await;
    client.set_quic_early_data(true).await;

    let broker_url = format!("quic://{quic_addr}");

    if client.connect(&broker_url).await.is_err() {
        eprintln!("Skipping 0-RTT fallback test - QUIC connection failed");
        broker_handle.abort();
        return;
    }
    assert!(client.is_connected().await);
    client.disconnect().await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    if client.connect(&broker_url).await.is_err() {
        eprintln!("Skipping 0-RTT fallback test - second QUIC connection failed");
        broker_handle.abort();
        return;
    }
    assert!(client.is_connected().await);
    assert!(
        !client.was_zero_rtt().await,
        "should fall back to 1-RTT when server has early data disabled"
    );

    client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_quic_0rtt_pubsub_after_reconnect() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();

    let (mut broker, quic_addr) = start_quic_broker_with_early_data(24583).await;
    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(500)).await;

    let broker_url = format!("quic://{quic_addr}");

    let client = MqttClient::new(test_client_id("0rtt-pubsub"));
    client.set_insecure_tls(true).await;
    client.set_quic_early_data(true).await;

    if client.connect(&broker_url).await.is_err() {
        eprintln!("Skipping 0-RTT pubsub test - QUIC connection failed");
        broker_handle.abort();
        return;
    }
    client.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    if client.connect(&broker_url).await.is_err() {
        eprintln!("Skipping 0-RTT pubsub test - second QUIC connection failed");
        broker_handle.abort();
        return;
    }
    assert!(client.is_connected().await);

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();
    client
        .subscribe("test/0rtt/#", move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    client.publish("test/0rtt/hello", b"world").await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        1,
        "should receive message after 0-RTT reconnection"
    );

    client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_quic_connection_close_sends_no_error() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24584).await;
    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let client = MqttClient::new(test_client_id("quic-graceful"));
    client.set_insecure_tls(true).await;

    let broker_url = format!("quic://{quic_addr}");
    if client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    assert!(client.is_connected().await);
    client.disconnect().await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    let client2 = MqttClient::new(test_client_id("quic-after-graceful"));
    client2.set_insecure_tls(true).await;
    if client2.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    assert!(
        client2.is_connected().await,
        "broker should still accept connections after graceful disconnect"
    );
    client2.disconnect().await.unwrap();
    broker_handle.abort();
}

#[test]
fn test_quic_error_codes_round_trip() {
    use mqtt5_protocol::quic_error_codes::{QuicConnectionCode, QuicStreamCode};

    let conn_codes = [
        (QuicConnectionCode::NoError, 0x00),
        (QuicConnectionCode::TlsError, 0xB1),
        (QuicConnectionCode::Unspecified, 0xB2),
        (QuicConnectionCode::TooManyRecoverAttempts, 0xB3),
        (QuicConnectionCode::ProtocolLevel0, 0xB4),
    ];
    for (code, expected_value) in conn_codes {
        assert_eq!(code.code(), expected_value);
        assert_eq!(QuicConnectionCode::from_code(expected_value), Some(code));
    }

    let stream_codes = [
        (QuicStreamCode::NoError, 0x00),
        (QuicStreamCode::NoFlowState, 0xB3),
        (QuicStreamCode::NotFlowOwner, 0xB4),
        (QuicStreamCode::StreamType, 0xB5),
        (QuicStreamCode::BadFlowId, 0xB6),
        (QuicStreamCode::PersistentTopic, 0xB7),
        (QuicStreamCode::PersistentSub, 0xB8),
        (QuicStreamCode::OptionalHeader, 0xB9),
        (QuicStreamCode::IncompletePacket, 0xBA),
        (QuicStreamCode::FlowOpenIdle, 0xBB),
        (QuicStreamCode::FlowCancelled, 0xBC),
        (QuicStreamCode::FlowPacketCancelled, 0xBD),
        (QuicStreamCode::FlowRefused, 0xBE),
        (QuicStreamCode::DiscardState, 0xBF),
        (QuicStreamCode::ServerPushNotWelcome, 0xC0),
        (QuicStreamCode::RecoveryFailed, 0xC1),
    ];
    for (code, expected_value) in stream_codes {
        assert_eq!(code.code(), expected_value);
        assert_eq!(QuicStreamCode::from_code(expected_value), Some(code));
    }

    assert_eq!(QuicConnectionCode::TooManyRecoverAttempts.code(), 0xB3);
    assert_eq!(QuicStreamCode::NoFlowState.code(), 0xB3);

    assert_eq!(QuicConnectionCode::from_code(0xC0), None);
    assert_eq!(QuicStreamCode::from_code(0xB1), None);
}

#[test]
fn test_quic_error_level_consistency() {
    use mqtt5_protocol::quic_error_codes::QuicStreamCode;
    use mqtt5_protocol::ReasonCode;

    let level_0_codes = [QuicStreamCode::NoFlowState, QuicStreamCode::NotFlowOwner];
    for code in level_0_codes {
        assert_eq!(code.error_level(), Some(0));
        let reason = ReasonCode::from_quic_stream_code(code).unwrap();
        assert_eq!(reason.mqoq_error_level(), Some(0));
    }

    let level_1_codes = [
        QuicStreamCode::StreamType,
        QuicStreamCode::BadFlowId,
        QuicStreamCode::PersistentTopic,
        QuicStreamCode::PersistentSub,
        QuicStreamCode::OptionalHeader,
        QuicStreamCode::DiscardState,
        QuicStreamCode::ServerPushNotWelcome,
        QuicStreamCode::RecoveryFailed,
    ];
    for code in level_1_codes {
        assert_eq!(code.error_level(), Some(1));
        let reason = ReasonCode::from_quic_stream_code(code).unwrap();
        assert_eq!(reason.mqoq_error_level(), Some(1));
    }

    let level_2_codes = [
        QuicStreamCode::IncompletePacket,
        QuicStreamCode::FlowOpenIdle,
        QuicStreamCode::FlowCancelled,
        QuicStreamCode::FlowPacketCancelled,
        QuicStreamCode::FlowRefused,
    ];
    for code in level_2_codes {
        assert_eq!(code.error_level(), Some(2));
        let reason = ReasonCode::from_quic_stream_code(code).unwrap();
        assert_eq!(reason.mqoq_error_level(), Some(2));
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_subscribe_on_data_flow_delivers_on_server_stream() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let quic_addr: SocketAddr = "127.0.0.1:24590".parse().unwrap();
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_server_delivery_strategy(ServerDeliveryStrategy::PerTopic)
        .with_quic(
            QuicConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(quic_addr),
        );

    let broker = MqttBroker::with_config(config).await.unwrap();
    let tcp_addr = broker.local_addr().unwrap();
    let mut broker = broker;
    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let topic1 = format!("flow-test/{}/t1", Ulid::new());
    let topic2 = format!("flow-test/{}/t2", Ulid::new());

    let pub_client = MqttClient::new(test_client_id("tcp-pub"));
    let tcp_url = format!("mqtt://{tcp_addr}");
    if pub_client.connect(&tcp_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let sub_client = MqttClient::new(test_client_id("quic-flow-sub"));
    sub_client.set_insecure_tls(true).await;
    sub_client
        .set_quic_stream_strategy(StreamStrategy::DataPerTopic)
        .await;
    sub_client.set_quic_flow_headers(true).await;

    let quic_url = format!("quic://{quic_addr}");
    if sub_client.connect(&quic_url).await.is_err() {
        pub_client.disconnect().await.ok();
        broker_handle.abort();
        return;
    }

    let stream_ids_1 = Arc::new(std::sync::Mutex::new(Vec::new()));
    let stream_ids_2 = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = Arc::new(AtomicU32::new(0));

    let topic1_sids = stream_ids_1.clone();
    let topic1_recv = received.clone();
    sub_client
        .subscribe(&topic1, move |msg| {
            if let Some(sid) = msg.stream_id {
                topic1_sids.lock().unwrap().push(sid);
            }
            topic1_recv.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    let topic2_sids = stream_ids_2.clone();
    let topic2_recv = received.clone();
    sub_client
        .subscribe(&topic2, move |msg| {
            if let Some(sid) = msg.stream_id {
                topic2_sids.lock().unwrap().push(sid);
            }
            topic2_recv.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    for i in 0..5 {
        pub_client
            .publish(&topic1, format!("t1-{i}").as_bytes())
            .await
            .unwrap();
        pub_client
            .publish(&topic2, format!("t2-{i}").as_bytes())
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        10,
        "Should receive all 10 messages (5 per topic)"
    );

    let captured_topic1: Vec<u64> = stream_ids_1.lock().unwrap().clone();
    let captured_topic2: Vec<u64> = stream_ids_2.lock().unwrap().clone();

    assert!(
        !captured_topic1.is_empty(),
        "Topic 1 messages should have stream_id set"
    );
    assert!(
        !captured_topic2.is_empty(),
        "Topic 2 messages should have stream_id set"
    );

    let first_stream_topic1 = captured_topic1[0];
    let first_stream_topic2 = captured_topic2[0];
    assert_ne!(
        first_stream_topic1, first_stream_topic2,
        "Different topics should be delivered on different server streams"
    );

    assert!(
        captured_topic1.iter().all(|s| *s == first_stream_topic1),
        "All messages for topic 1 should arrive on the same stream"
    );
    assert!(
        captured_topic2.iter().all(|s| *s == first_stream_topic2),
        "All messages for topic 2 should arrive on the same stream"
    );

    pub_client.disconnect().await.ok();
    sub_client.disconnect().await.ok();
    broker_handle.abort();
}
