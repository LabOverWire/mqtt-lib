use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::timeout;

fn test_broker_config() -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(100)
        .with_storage(StorageConfig::new().with_persistence(false))
}

async fn wait_for_ready(mut rx: tokio::sync::watch::Receiver<bool>) {
    while !*rx.borrow_and_update() {
        rx.changed().await.expect("broker ready channel closed");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bridge_with_try_private_true() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("mqtt5=debug,warn")
        .try_init();

    let broker2_config = test_broker_config();

    let mut broker2 = MqttBroker::with_config(broker2_config).await.unwrap();
    let broker2_addr = broker2.local_addr().expect("Failed to get broker2 address");
    let broker2_ready = broker2.ready_receiver();

    let broker2_handle = tokio::spawn(async move {
        if let Err(e) = broker2.run().await {
            eprintln!("broker2 run() error: {e}");
        }
    });

    wait_for_ready(broker2_ready).await;

    let mut bridge_config = BridgeConfig::new("test-bridge", broker2_addr.to_string()).add_topic(
        "sensors/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    bridge_config.try_private = true;

    let mut broker1_config = test_broker_config();
    broker1_config.bridges = vec![bridge_config];

    let mut broker1 = MqttBroker::with_config(broker1_config).await.unwrap();
    let broker1_addr = broker1.local_addr().expect("Failed to get broker1 address");
    let broker1_ready = broker1.ready_receiver();

    let broker1_handle = tokio::spawn(async move {
        if let Err(e) = broker1.run().await {
            eprintln!("broker1 run() error: {e}");
        }
    });

    wait_for_ready(broker1_ready).await;

    let client1 = MqttClient::new("publisher");
    let client2 = MqttClient::new("subscriber");

    client1
        .connect(&format!("mqtt://{broker1_addr}"))
        .await
        .expect("client1 connect failed");
    client2
        .connect(&format!("mqtt://{broker2_addr}"))
        .await
        .expect("client2 connect failed");

    let (tx, mut rx) = mpsc::channel(10);

    client2
        .subscribe("sensors/#", move |msg| {
            let _ = tx.try_send((msg.topic.clone(), msg.payload.clone()));
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    client1
        .publish_qos1("sensors/temp/data", b"25.5C")
        .await
        .unwrap();

    let result = timeout(Duration::from_secs(3), rx.recv()).await;
    assert!(result.is_ok(), "Should receive message via bridge");

    let (topic, payload) = result.unwrap().unwrap();
    assert_eq!(topic, "sensors/temp/data");
    assert_eq!(payload, b"25.5C");

    client1.disconnect().await.unwrap();
    client2.disconnect().await.unwrap();

    broker1_handle.abort();
    broker2_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bridge_with_try_private_false() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("mqtt5=debug,warn")
        .try_init();

    let broker2_config = test_broker_config();

    let mut broker2 = MqttBroker::with_config(broker2_config).await.unwrap();
    let broker2_addr = broker2.local_addr().expect("Failed to get broker2 address");
    let broker2_ready = broker2.ready_receiver();

    let broker2_handle = tokio::spawn(async move {
        if let Err(e) = broker2.run().await {
            eprintln!("broker2 run() error: {e}");
        }
    });

    wait_for_ready(broker2_ready).await;

    let mut bridge_config = BridgeConfig::new("test-bridge-no-private", broker2_addr.to_string())
        .add_topic("commands/#", BridgeDirection::Out, QoS::AtLeastOnce);
    bridge_config.try_private = false;

    let mut broker1_config = test_broker_config();
    broker1_config.bridges = vec![bridge_config];

    let mut broker1 = MqttBroker::with_config(broker1_config).await.unwrap();
    let broker1_addr = broker1.local_addr().expect("Failed to get broker1 address");
    let broker1_ready = broker1.ready_receiver();

    let broker1_handle = tokio::spawn(async move {
        if let Err(e) = broker1.run().await {
            eprintln!("broker1 run() error: {e}");
        }
    });

    wait_for_ready(broker1_ready).await;

    let client1 = MqttClient::new("publisher2");
    let client2 = MqttClient::new("subscriber2");

    client1
        .connect(&format!("mqtt://{broker1_addr}"))
        .await
        .expect("client1 connect failed");
    client2
        .connect(&format!("mqtt://{broker2_addr}"))
        .await
        .expect("client2 connect failed");

    let (tx, mut rx) = mpsc::channel(10);

    client2
        .subscribe("commands/#", move |msg| {
            let _ = tx.try_send((msg.topic.clone(), msg.payload.clone()));
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    client1
        .publish_qos1("commands/hvac/on", b"true")
        .await
        .unwrap();

    let result = timeout(Duration::from_secs(3), rx.recv()).await;
    assert!(
        result.is_ok(),
        "Should receive message via bridge even with try_private=false"
    );

    let (topic, payload) = result.unwrap().unwrap();
    assert_eq!(topic, "commands/hvac/on");
    assert_eq!(payload, b"true");

    client1.disconnect().await.unwrap();
    client2.disconnect().await.unwrap();

    broker1_handle.abort();
    broker2_handle.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_bridge_bidirectional_with_try_private() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("mqtt5=debug,warn")
        .try_init();

    let broker2_config = test_broker_config();

    let mut broker2 = MqttBroker::with_config(broker2_config).await.unwrap();
    let broker2_addr = broker2.local_addr().expect("Failed to get broker2 address");
    let broker2_ready = broker2.ready_receiver();

    let broker2_handle = tokio::spawn(async move {
        if let Err(e) = broker2.run().await {
            eprintln!("broker2 run() error: {e}");
        }
    });

    wait_for_ready(broker2_ready).await;

    let mut bridge_config = BridgeConfig::new("bidirectional-bridge", broker2_addr.to_string())
        .add_topic("status/#", BridgeDirection::Both, QoS::AtLeastOnce);
    bridge_config.try_private = true;

    let mut broker1_config = test_broker_config();
    broker1_config.bridges = vec![bridge_config];

    let mut broker1 = MqttBroker::with_config(broker1_config).await.unwrap();
    let broker1_addr = broker1.local_addr().expect("Failed to get broker1 address");
    let broker1_ready = broker1.ready_receiver();

    let broker1_handle = tokio::spawn(async move {
        if let Err(e) = broker1.run().await {
            eprintln!("broker1 run() error: {e}");
        }
    });

    wait_for_ready(broker1_ready).await;

    let client1 = Arc::new(MqttClient::new("client-on-broker1"));
    let client2 = Arc::new(MqttClient::new("client-on-broker2"));

    client1
        .connect(&format!("mqtt://{broker1_addr}"))
        .await
        .expect("client1 connect failed");
    client2
        .connect(&format!("mqtt://{broker2_addr}"))
        .await
        .expect("client2 connect failed");

    let (tx1, mut rx1) = mpsc::channel(10);
    let (tx2, mut rx2) = mpsc::channel(10);

    client1
        .subscribe("status/#", move |msg| {
            let _ = tx1.try_send((msg.topic.clone(), msg.payload.clone()));
        })
        .await
        .unwrap();

    client2
        .subscribe("status/#", move |msg| {
            let _ = tx2.try_send((msg.topic.clone(), msg.payload.clone()));
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;

    client1
        .publish_qos1("status/broker1", b"online")
        .await
        .unwrap();

    let local_msg = timeout(Duration::from_millis(100), rx1.recv()).await;
    assert!(local_msg.is_ok(), "Should receive local message");

    let result = timeout(Duration::from_secs(3), rx2.recv()).await;
    assert!(
        result.is_ok(),
        "Broker2 client should receive message from broker1"
    );

    let (topic, payload) = result.unwrap().unwrap();
    assert_eq!(topic, "status/broker1");
    assert_eq!(payload, b"online");

    client2
        .publish_qos1("status/broker2", b"ready")
        .await
        .unwrap();

    let local_msg = timeout(Duration::from_millis(100), rx2.recv()).await;
    assert!(local_msg.is_ok(), "Should receive local message");

    let mut found = false;
    for _ in 0..3 {
        let result = timeout(Duration::from_secs(1), rx1.recv()).await;
        if let Ok(Some((topic, payload))) = result {
            if topic == "status/broker2" && payload == b"ready" {
                found = true;
                break;
            }
        }
    }
    assert!(found, "Broker1 client should receive message from broker2");

    client1.disconnect().await.unwrap();
    client2.disconnect().await.unwrap();

    broker1_handle.abort();
    broker2_handle.abort();
}
