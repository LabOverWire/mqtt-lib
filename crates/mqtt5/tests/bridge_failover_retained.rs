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

fn remote_config(bind: SocketAddr, ids: bool) -> BrokerConfig {
    let mut config = broker_config(bind);
    config.subscription_identifier_available = ids;
    config
}

fn count(received: &Arc<Mutex<Vec<(String, u8)>>>, topic: &str) -> usize {
    received
        .lock()
        .unwrap()
        .iter()
        .filter(|(t, _)| t == topic)
        .count()
}

async fn publish_retained(remote: SocketAddr, topic: &str, tag: &str) {
    let publisher = MqttClient::new(format!("pub-{tag}"));
    publisher
        .connect(&format!("mqtt://{remote}"))
        .await
        .unwrap();
    publisher
        .publish_retain(topic, b"r".to_vec())
        .await
        .unwrap();
    publisher.disconnect().await.unwrap();
}

async fn backup_retained_copies_after_failover(primary_ids: bool, backup_ids: bool) -> usize {
    let primary_bind = free_port();
    let primary = start_broker(remote_config(primary_bind, primary_ids)).await;
    let backup = start_broker(remote_config(free_port(), backup_ids)).await;
    publish_retained(
        backup.addr,
        "sensors/backup",
        &format!("rb-{primary_ids}-{backup_ids}"),
    )
    .await;
    let mut bridge = BridgeConfig::new("failover", primary_bind.to_string())
        .add_topic("sensors/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_backup_broker(backup.addr.to_string());
    bridge.enable_failback = false;
    bridge.first_retry_delay = Duration::from_millis(200);
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    bridge.max_reconnect_delay = Duration::from_millis(500);
    bridge.retry_jitter = false;
    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![bridge];
    let local = start_broker(local_config).await;
    let local_client = MqttClient::new(format!("local-{primary_ids}-{backup_ids}"));
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "#").await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    primary.stop().await;
    wait_until(Duration::from_secs(5), || {
        count(&received, "sensors/backup") > 0
    })
    .await;
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let copies = count(&received, "sensors/backup");
    local_client.disconnect().await.unwrap();
    local.stop().await;
    backup.stop().await;
    copies
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_retained_once_after_failover_both_identifiers() {
    assert_eq!(backup_retained_copies_after_failover(true, true).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_retained_once_after_failover_without_identifiers() {
    assert_eq!(backup_retained_copies_after_failover(false, false).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_retained_once_after_failover_losing_identifiers() {
    assert_eq!(backup_retained_copies_after_failover(true, false).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backup_retained_once_after_failover_gaining_identifiers() {
    assert_eq!(backup_retained_copies_after_failover(false, true).await, 1);
}
