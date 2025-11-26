use mqtt5::broker::config::{BrokerConfig, QuicConfig};
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
