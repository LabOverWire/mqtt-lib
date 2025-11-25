use mqtt5::time::Duration;
use mqtt5::transport::StreamStrategy;
use mqtt5::{MqttClient, QoS, SubscribeOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::time::{sleep, timeout};
use ulid::Ulid;

const EMQX_BROKER: &str = "quic://localhost:14567";

fn test_client_id(prefix: &str) -> String {
    format!("{}-{}", prefix, Ulid::new())
}

fn test_topic(test_name: &str) -> String {
    format!("quic-test/{}/{}", Ulid::new(), test_name)
}

#[tokio::test]
async fn test_quic_basic_connection() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client_id = test_client_id("quic-basic");
    let client = MqttClient::new(client_id);

    let result = timeout(Duration::from_secs(10), client.connect(EMQX_BROKER)).await;

    assert!(result.is_ok(), "Connection timeout");
    let connect_result = result.unwrap();
    assert!(
        connect_result.is_ok(),
        "QUIC connection failed: {:?}",
        connect_result.err()
    );

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_basic_pubsub() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pub_client_id = test_client_id("quic-pub");
    let sub_client_id = test_client_id("quic-sub");
    let topic = test_topic("basic");

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.connect(EMQX_BROKER).await.unwrap();
    sub_client.connect(EMQX_BROKER).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(200)).await;

    pub_client.publish(&topic, b"Hello QUIC").await.unwrap();

    sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        1,
        "Should receive exactly 1 message"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_qos0_fire_and_forget() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pub_client_id = test_client_id("quic-qos0-pub");
    let sub_client_id = test_client_id("quic-qos0-sub");
    let topic = test_topic("qos0");

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.connect(EMQX_BROKER).await.unwrap();
    sub_client.connect(EMQX_BROKER).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(200)).await;

    for i in 0..10 {
        pub_client
            .publish_qos0(&topic, format!("QoS 0 Message {i}"))
            .await
            .unwrap();
    }

    sleep(Duration::from_secs(1)).await;

    let count = received.load(Ordering::Relaxed);
    println!("QoS 0: Received {count} of 10 messages");
    assert!(count > 0, "Should receive at least some messages");

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_qos1_at_least_once() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pub_client_id = test_client_id("quic-qos1-pub");
    let sub_client_id = test_client_id("quic-qos1-sub");
    let topic = test_topic("qos1");

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.connect(EMQX_BROKER).await.unwrap();
    sub_client.connect(EMQX_BROKER).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe_with_options(
            &topic,
            SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |_msg| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    sleep(Duration::from_millis(200)).await;

    for i in 0..5 {
        pub_client
            .publish_qos1(&topic, format!("QoS 1 Message {i}"))
            .await
            .unwrap();
    }

    sleep(Duration::from_secs(2)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        5,
        "Should receive exactly 5 QoS 1 messages"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_qos2_exactly_once() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let pub_client_id = test_client_id("quic-qos2-pub");
    let sub_client_id = test_client_id("quic-qos2-sub");
    let topic = test_topic("qos2");

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.connect(EMQX_BROKER).await.unwrap();
    sub_client.connect(EMQX_BROKER).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe_with_options(
            &topic,
            SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |_msg| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    sleep(Duration::from_millis(200)).await;

    for i in 0..3 {
        pub_client
            .publish_qos2(&topic, format!("QoS 2 Message {i}"))
            .await
            .unwrap();
    }

    sleep(Duration::from_secs(3)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        3,
        "Should receive exactly 3 QoS 2 messages"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_control_only_strategy() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client_id = test_client_id("quic-control-only");
    let topic = test_topic("control-only");

    let client = MqttClient::new(client_id);

    client
        .set_quic_stream_strategy(StreamStrategy::ControlOnly)
        .await;

    client.connect(EMQX_BROKER).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(200)).await;

    for i in 0..5 {
        client
            .publish_qos1(&topic, format!("Control Only Message {i}"))
            .await
            .unwrap();
    }

    sleep(Duration::from_secs(2)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        5,
        "ControlOnly strategy: should receive all messages"
    );

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_data_per_publish_strategy() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client_id = test_client_id("quic-data-per-publish");
    let topic = test_topic("data-per-publish");

    let client = MqttClient::new(client_id);

    client
        .set_quic_stream_strategy(StreamStrategy::DataPerPublish)
        .await;

    client.connect(EMQX_BROKER).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(200)).await;

    for i in 0..5 {
        client
            .publish_qos1(&topic, format!("DataPerPublish Message {i}"))
            .await
            .unwrap();
    }

    sleep(Duration::from_secs(2)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        5,
        "DataPerPublish strategy: should receive all messages on dedicated streams"
    );

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_data_per_topic_strategy() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client_id = test_client_id("quic-data-per-topic");
    let topic1 = test_topic("topic-a");
    let topic2 = test_topic("topic-b");

    let client = MqttClient::new(client_id);

    client
        .set_quic_stream_strategy(StreamStrategy::DataPerTopic)
        .await;

    client.connect(EMQX_BROKER).await.unwrap();

    let received_a = Arc::new(AtomicU32::new(0));
    let received_b = Arc::new(AtomicU32::new(0));
    let received_a_clone = received_a.clone();
    let received_b_clone = received_b.clone();

    client
        .subscribe(&topic1, move |_msg| {
            received_a_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    client
        .subscribe(&topic2, move |_msg| {
            received_b_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(200)).await;

    for i in 0..3 {
        client
            .publish_qos1(&topic1, format!("Topic A Message {i}"))
            .await
            .unwrap();
        client
            .publish_qos1(&topic2, format!("Topic B Message {i}"))
            .await
            .unwrap();
    }

    sleep(Duration::from_secs(2)).await;

    assert_eq!(
        received_a.load(Ordering::Relaxed),
        3,
        "DataPerTopic strategy: should receive all topic A messages"
    );
    assert_eq!(
        received_b.load(Ordering::Relaxed),
        3,
        "DataPerTopic strategy: should receive all topic B messages"
    );

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_concurrent_publishes() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client_id = test_client_id("quic-concurrent");
    let topic = test_topic("concurrent");

    let client = MqttClient::new(client_id);

    client
        .set_quic_stream_strategy(StreamStrategy::DataPerPublish)
        .await;

    client.connect(EMQX_BROKER).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(200)).await;

    let mut handles = vec![];
    for i in 0..10 {
        let client_clone = client.clone();
        let topic_clone = topic.clone();
        let handle = tokio::spawn(async move {
            client_clone
                .publish_qos1(&topic_clone, format!("Concurrent Message {i}"))
                .await
                .unwrap();
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap();
    }

    sleep(Duration::from_secs(3)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        10,
        "Should receive all 10 concurrent messages"
    );

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_large_message() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client_id = test_client_id("quic-large");
    let topic = test_topic("large");

    let client = MqttClient::new(client_id);

    client.connect(EMQX_BROKER).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(200)).await;

    let large_payload = vec![0u8; 10_000];
    client.publish_qos1(&topic, large_payload).await.unwrap();

    sleep(Duration::from_secs(2)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        1,
        "Should receive large message"
    );

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_quic_reconnect() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let client_id = test_client_id("quic-reconnect");

    let client = MqttClient::new(&client_id);

    client.connect(EMQX_BROKER).await.unwrap();

    client.disconnect().await.unwrap();

    sleep(Duration::from_millis(500)).await;

    let result = client.connect(EMQX_BROKER).await;

    assert!(result.is_ok(), "Should successfully reconnect");

    client.disconnect().await.unwrap();
}
