#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::config::StorageBackend;
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

struct RunningBroker {
    addr: SocketAddr,
    task: JoinHandle<()>,
    shutdown: BrokerShutdownHandle,
}

impl RunningBroker {
    async fn stop(self) {
        self.shutdown.shutdown();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.task).await;
    }
}

fn free_port() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

async fn start_broker(config: BrokerConfig) -> RunningBroker {
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    while !*ready.borrow_and_update() {
        ready.changed().await.unwrap();
    }
    RunningBroker {
        addr,
        task,
        shutdown,
    }
}

fn local_config(remote: SocketAddr, order: &[(&str, &str)]) -> BrokerConfig {
    let mut bridge = BridgeConfig::new("reordered", remote.to_string());
    for (filter, prefix) in order {
        bridge = bridge.add_topic(*filter, BridgeDirection::In, QoS::AtLeastOnce);
        if let Some(mapping) = bridge.topics.last_mut() {
            mapping.local_prefix = Some((*prefix).to_string());
        }
    }
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    bridge.first_retry_delay = Duration::from_millis(100);
    bridge.max_reconnect_delay = Duration::from_millis(300);
    let mut config = BrokerConfig::default()
        .with_bind_address(free_port())
        .with_storage(StorageConfig::new().with_persistence(false));
    config.bridges = vec![bridge];
    config
}

async fn gated_proxy(listener: TcpListener, upstream: SocketAddr, open: Arc<AtomicBool>) {
    while let Ok((mut inbound, _)) = listener.accept().await {
        let open = Arc::clone(&open);
        tokio::spawn(async move {
            while !open.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            if let Ok(mut outbound) = TcpStream::connect(upstream).await {
                let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
            }
        });
    }
}

async fn watch(local: SocketAddr, name: &str) -> (MqttClient, Arc<Mutex<Vec<String>>>) {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let client = MqttClient::new(name);
    client.connect(&format!("mqtt://{local}")).await.unwrap();
    client
        .subscribe("#", move |msg| sink.lock().unwrap().push(msg.topic.clone()))
        .await
        .unwrap();
    (client, received)
}

async fn wait_for(received: &Arc<Mutex<Vec<String>>>, matches: impl Fn(&str) -> bool) -> bool {
    for _ in 0..150 {
        if received.lock().unwrap().iter().any(|t| matches(t)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

async fn queued_routing_after_restart(
    first_order: &[(&str, &str)],
    second_order: &[(&str, &str)],
) -> Vec<String> {
    let remote = start_broker(
        BrokerConfig::default()
            .with_bind_address(free_port())
            .with_storage(StorageConfig {
                backend: StorageBackend::Memory,
                enable_persistence: true,
                ..Default::default()
            }),
    )
    .await;
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = proxy_listener.local_addr().unwrap();
    let open = Arc::new(AtomicBool::new(true));
    let proxy_task = tokio::spawn(gated_proxy(proxy_listener, remote.addr, Arc::clone(&open)));
    let publisher = MqttClient::new("reordered-publisher");
    publisher
        .connect(&format!("mqtt://{}", remote.addr))
        .await
        .unwrap();

    let first = start_broker(local_config(proxy, first_order)).await;
    let (first_watcher, first_received) = watch(first.addr, "first-watcher").await;
    let mut probed = false;
    for _ in 0..25 {
        publisher
            .publish_qos1("a/probe", b"p".to_vec())
            .await
            .unwrap();
        if wait_for(&first_received, |t| t == "fromA/a/probe").await {
            probed = true;
            break;
        }
    }
    let _ = first_watcher.disconnect().await;
    first.stop().await;
    open.store(false, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    publisher.publish_qos1("a/x", b"x".to_vec()).await.unwrap();

    let second = start_broker(local_config(proxy, second_order)).await;
    let (second_watcher, second_received) = watch(second.addr, "second-watcher").await;
    open.store(true, Ordering::SeqCst);
    let delivered = wait_for(&second_received, |t| t.ends_with("/a/x")).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let topics: Vec<String> = second_received
        .lock()
        .unwrap()
        .iter()
        .filter(|t| t.ends_with("/a/x"))
        .cloned()
        .collect();
    let _ = second_watcher.disconnect().await;
    let _ = publisher.disconnect().await;
    second.stop().await;
    proxy_task.abort();
    remote.stop().await;

    assert!(
        probed,
        "setup: the bridge never delivered through the first order"
    );
    assert!(
        delivered,
        "setup: the queued a/x never reached the restarted bridge"
    );
    topics
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_message_after_restart_with_reordered_mappings_keeps_its_mapping() {
    let topics = queued_routing_after_restart(
        &[("a/#", "fromA/"), ("b/#", "fromB/")],
        &[("b/#", "fromB/"), ("a/#", "fromA/")],
    )
    .await;
    assert_eq!(
        topics,
        vec!["fromA/a/x".to_string()],
        "a/x was queued for the a/# mapping before the restart, but after the mappings were reordered it was routed as {topics:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_message_after_restart_skips_a_newly_added_overlapping_mapping() {
    let topics = queued_routing_after_restart(
        &[("a/#", "fromA/"), ("x/#", "fromX/")],
        &[("a/+", "fromPlus/"), ("a/#", "fromA/")],
    )
    .await;
    assert_eq!(
        topics,
        vec!["fromA/a/x".to_string()],
        "a/x was queued for the a/# mapping before a/+ was added in front of it, but it was routed as {topics:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queued_message_whose_filter_is_gone_after_restart_is_routed_by_topic() {
    let topics = queued_routing_after_restart(
        &[("a/#", "fromA/"), ("x/#", "fromX/")],
        &[("a/+", "fromPlus/"), ("x/#", "fromX/")],
    )
    .await;
    assert_eq!(
        topics,
        vec!["fromPlus/a/x".to_string()],
        "a/x was queued under the a/# filter the restarted bridge no longer has, and a/+ matches it, but it was routed as {topics:?}"
    );
}
