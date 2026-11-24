#![cfg(feature = "broker")]
use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection, TopicMapping};
use mqtt5::broker::{BrokerConfig, BrokerShutdownHandle, MqttBroker, StorageConfig};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

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
            .expect("broker stops")
            .expect("broker joins");
    }
}

async fn start_broker(config: BrokerConfig) -> RunningBroker {
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let task = tokio::spawn(async move {
        let outcome = broker.run().await;
        assert!(outcome.is_ok(), "broker run failed: {outcome:?}");
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

fn free_port() -> SocketAddr {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn remote_config(bind: SocketAddr, dir: &Path, ids: bool) -> BrokerConfig {
    let mut config = BrokerConfig::default()
        .with_bind_address(bind)
        .with_max_clients(100)
        .with_storage(
            StorageConfig::new()
                .with_persistence(true)
                .with_base_dir(dir.to_path_buf()),
        );
    config.subscription_identifier_available = ids;
    config
}

fn mapping(pattern: &str, local_prefix: &str) -> TopicMapping {
    TopicMapping {
        pattern: pattern.to_string(),
        direction: BridgeDirection::In,
        qos: QoS::AtLeastOnce,
        local_prefix: Some(local_prefix.to_string()),
        remote_prefix: None,
    }
}

fn count(received: &Arc<Mutex<Vec<String>>>, topic: &str) -> usize {
    received
        .lock()
        .unwrap()
        .iter()
        .filter(|t| *t == topic)
        .count()
}

async fn copies_after_restart(first_ids: bool, second_ids: bool) -> (usize, usize) {
    let dir = tempfile::tempdir().unwrap();
    let bind = free_port();
    let remote = start_broker(remote_config(bind, dir.path(), first_ids)).await;
    let publisher = MqttClient::new(format!("retained-mode-switch-pub-{first_ids}-{second_ids}"));
    publisher.connect(&format!("mqtt://{bind}")).await.unwrap();
    publisher
        .publish_retain("sensors/a/temp", b"r".to_vec())
        .await
        .unwrap();
    publisher.disconnect().await.unwrap();

    let mut bridge = BridgeConfig::new("retained-mode-switch", bind.to_string());
    bridge.client_id = format!("retained-mode-switch-bridge-{first_ids}-{second_ids}");
    bridge.topics = vec![
        mapping("sensors/#", "all/"),
        mapping("sensors/+/temp", "temp/"),
    ];
    bridge.first_retry_delay = Duration::from_millis(500);
    bridge.initial_reconnect_delay = Duration::from_millis(500);
    bridge.max_reconnect_delay = Duration::from_millis(500);
    bridge.retry_jitter = false;
    let mut local_config = BrokerConfig::default()
        .with_bind_address(free_port())
        .with_max_clients(100)
        .with_storage(StorageConfig::new().with_persistence(false));
    local_config.bridges = vec![bridge];
    let local = start_broker(local_config).await;

    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let local_client = MqttClient::new(format!(
        "retained-mode-switch-local-{first_ids}-{second_ids}"
    ));
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    local_client
        .subscribe("#", move |msg| sink.lock().unwrap().push(msg.topic.clone()))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(2000)).await;
    let before = (
        count(&received, "all/sensors/a/temp"),
        count(&received, "temp/sensors/a/temp"),
    );
    assert!(
        before.0 > 0 && before.1 > 0,
        "first connection delivers the retained message"
    );

    remote.stop().await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    let remote = start_broker(remote_config(bind, dir.path(), second_ids)).await;
    let prober = MqttClient::new(format!(
        "retained-mode-switch-probe-{first_ids}-{second_ids}"
    ));
    prober.connect(&format!("mqtt://{bind}")).await.unwrap();
    let mut reconnected = false;
    for _ in 0..50 {
        prober
            .publish_qos1("sensors/probe/temp", b"p".to_vec())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        if count(&received, "all/sensors/probe/temp") > 0
            && count(&received, "temp/sensors/probe/temp") > 0
        {
            reconnected = true;
            break;
        }
    }
    prober.disconnect().await.unwrap();
    assert!(
        reconnected,
        "setup: the bridge never delivered through the restarted remote"
    );
    tokio::time::sleep(Duration::from_millis(1000)).await;

    let copies = (
        count(&received, "all/sensors/a/temp") - before.0,
        count(&received, "temp/sensors/a/temp") - before.1,
    );
    local_client.disconnect().await.unwrap();
    local.stop().await;
    remote.stop().await;
    copies
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_session_same_mode_control() {
    assert_eq!(copies_after_restart(false, false).await, (0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_session_with_identifiers_control() {
    assert_eq!(copies_after_restart(true, true).await, (0, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resumed_session_gaining_identifiers_does_not_resend_retained() {
    assert_eq!(
        copies_after_restart(false, true).await,
        (0, 0),
        "retained message reached the local broker again after the remote kept the session and gained identifiers"
    );
}
