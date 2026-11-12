#![cfg(feature = "broker")]
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

fn broker_config(bind: SocketAddr) -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address(bind)
        .with_max_clients(100)
        .with_storage(StorageConfig::new().with_persistence(false))
}

async fn wait_for_ready(mut rx: tokio::sync::watch::Receiver<bool>) {
    while !*rx.borrow_and_update() {
        rx.changed().await.expect("broker ready channel closed");
    }
}

struct RunningBroker {
    addr: SocketAddr,
    task: JoinHandle<()>,
    shutdown: BrokerShutdownHandle,
}

impl RunningBroker {
    async fn stop(self) {
        self.shutdown.shutdown();
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("broker stops on shutdown")
            .expect("broker task joins");
    }
}

async fn start_broker(config: BrokerConfig) -> RunningBroker {
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        if let Err(e) = broker.run().await {
            eprintln!("broker run() error: {e}");
        }
    });
    wait_for_ready(ready).await;
    RunningBroker {
        addr,
        task,
        shutdown,
    }
}

fn free_port() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

async fn wait_until(deadline: Duration, cond: impl Fn() -> bool) -> bool {
    let started = tokio::time::Instant::now();
    while started.elapsed() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

async fn subscribe_all(client: &MqttClient, filter: &str) -> Arc<Mutex<Vec<(String, u8)>>> {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    client
        .subscribe_with_options(
            filter,
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |msg| {
                sink.lock()
                    .unwrap()
                    .push((msg.topic.clone(), msg.qos as u8));
            },
        )
        .await
        .unwrap();
    received
}

fn fast(mut config: BridgeConfig) -> BridgeConfig {
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.first_retry_delay = Duration::from_millis(200);
    config.max_reconnect_delay = Duration::from_millis(500);
    config
}

fn remote_config(bind: SocketAddr, ids: bool) -> BrokerConfig {
    let mut config = broker_config(bind);
    config.subscription_identifier_available = ids;
    config
}

async fn publish_until_delivered(
    remote: SocketAddr,
    topic: &str,
    received: &Arc<Mutex<Vec<(String, u8)>>>,
    tag: &str,
) -> bool {
    let publisher = MqttClient::new(format!("pub-{tag}"));
    publisher
        .connect(&format!("mqtt://{remote}"))
        .await
        .unwrap();
    let before = received.lock().unwrap().len();
    let mut delivered = false;
    for _ in 0..25 {
        publisher.publish_qos1(topic, b"x".to_vec()).await.unwrap();
        if wait_until(Duration::from_millis(200), || {
            received.lock().unwrap().len() > before
        })
        .await
        {
            delivered = true;
            break;
        }
    }
    publisher.disconnect().await.unwrap();
    delivered
}

async fn shared_filter_case(ids: bool) -> bool {
    let remote = start_broker(remote_config(free_port(), ids)).await;
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![fast(
        BridgeConfig::new("shared", remote.addr.to_string()).add_topic(
            "$share/g/sensors/#",
            BridgeDirection::In,
            QoS::AtLeastOnce,
        ),
    )];
    let local = start_broker(local_config).await;
    let local_client = MqttClient::new(format!("local-shared-{ids}"));
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "#").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let delivered = publish_until_delivered(
        remote.addr,
        "sensors/x",
        &received,
        &format!("shared-{ids}"),
    )
    .await;
    local_client.disconnect().await.unwrap();
    local.stop().await;
    remote.stop().await;
    delivered
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_filter_control_with_identifiers() {
    assert!(
        shared_filter_case(true).await,
        "shared mapping delivers with identifiers"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_filter_without_identifiers_delivers() {
    assert!(
        shared_filter_case(false).await,
        "a $share mapping must deliver a local copy from a remote without subscription identifiers"
    );
}

async fn capability_change_case(first_ids: bool, second_ids: bool) -> (bool, bool) {
    let bind = free_port();
    let remote = start_broker(remote_config(bind, first_ids)).await;
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![fast(
        BridgeConfig::new("change", bind.to_string()).add_topic(
            "sensors/#",
            BridgeDirection::In,
            QoS::AtLeastOnce,
        ),
    )];
    let local = start_broker(local_config).await;
    let local_client = MqttClient::new(format!("local-change-{first_ids}-{second_ids}"));
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "#").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = publish_until_delivered(bind, "sensors/a", &received, "before").await;
    remote.stop().await;
    let remote = start_broker(remote_config(bind, second_ids)).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let after = publish_until_delivered(bind, "sensors/b", &received, "after").await;
    local_client.disconnect().await.unwrap();
    local.stop().await;
    remote.stop().await;
    (before, after)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_loses_identifier_support_after_lost_session() {
    let (before, after) = capability_change_case(true, false).await;
    assert!(before, "delivers before the change");
    assert!(
        after,
        "delivers after reconnecting to a remote without identifiers (session present 0)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_gains_identifier_support_after_lost_session() {
    let (before, after) = capability_change_case(false, true).await;
    assert!(before, "delivers before the change");
    assert!(
        after,
        "delivers after reconnecting to a remote with identifiers"
    );
}

async fn failover_case(backup_ids: bool) -> (bool, bool) {
    let primary_bind = free_port();
    let primary = start_broker(remote_config(primary_bind, true)).await;
    let backup = start_broker(remote_config(free_port(), backup_ids)).await;
    let mut bridge = fast(
        BridgeConfig::new("failover", primary_bind.to_string())
            .add_topic("sensors/#", BridgeDirection::In, QoS::AtLeastOnce)
            .add_backup_broker(backup.addr.to_string()),
    );
    bridge.enable_failback = false;
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![bridge];
    let local = start_broker(local_config).await;
    let local_client = MqttClient::new(format!("local-failover-{backup_ids}"));
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "#").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = publish_until_delivered(primary_bind, "sensors/a", &received, "fo-before").await;
    primary.stop().await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let after = publish_until_delivered(backup.addr, "sensors/b", &received, "fo-after").await;
    local_client.disconnect().await.unwrap();
    local.stop().await;
    backup.stop().await;
    (before, after)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failover_to_backup_with_identifiers_control() {
    let (before, after) = failover_case(true).await;
    assert!(before);
    assert!(after, "delivers from a backup with identifiers");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failover_to_backup_without_identifiers() {
    let (before, after) = failover_case(false).await;
    assert!(before);
    assert!(
        after,
        "delivers from a backup without identifiers after failover"
    );
}

async fn qos_case(ids: bool) -> Vec<(String, u8)> {
    let remote = start_broker(remote_config(free_port(), ids)).await;
    let mut config = fast(
        BridgeConfig::new("qos", remote.addr.to_string())
            .add_topic("sensors/#", BridgeDirection::In, QoS::AtMostOnce)
            .add_topic("sensors/+/temp", BridgeDirection::In, QoS::ExactlyOnce),
    );
    config.topics[0].local_prefix = Some("a/".to_string());
    config.topics[1].local_prefix = Some("b/".to_string());
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![config];
    let local = start_broker(local_config).await;
    let local_client = MqttClient::new(format!("local-qos-{ids}"));
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "#").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let publisher = MqttClient::new(format!("pub-qos-{ids}"));
    publisher
        .connect(&format!("mqtt://{}", remote.addr))
        .await
        .unwrap();
    publisher
        .publish_qos1("sensors/x/humidity", b"h".to_vec())
        .await
        .unwrap();
    wait_until(Duration::from_secs(3), || {
        !received.lock().unwrap().is_empty()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = received.lock().unwrap().clone();
    publisher.disconnect().await.unwrap();
    local_client.disconnect().await.unwrap();
    local.stop().await;
    remote.stop().await;
    got
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qos_of_lower_mapping_without_identifiers() {
    let with_ids = qos_case(true).await;
    let without_ids = qos_case(false).await;
    assert_eq!(
        with_ids, without_ids,
        "the QoS 0 mapping's local copy has the same QoS with and without identifiers"
    );
}
