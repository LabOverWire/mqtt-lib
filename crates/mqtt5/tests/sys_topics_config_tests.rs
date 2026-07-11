mod common;

use common::TestBroker;
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::MqttClient;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn base_config() -> BrokerConfig {
    let storage = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage)
}

async fn collect_sys_topics(address: &str, wait: Duration) -> Vec<String> {
    let client = MqttClient::new("sys-sub");
    client.connect(address).await.expect("connect");

    let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    client
        .subscribe("$SYS/#", move |msg| {
            sink.lock().unwrap().push(msg.topic.clone());
        })
        .await
        .expect("subscribe");

    tokio::time::sleep(wait).await;
    client.disconnect().await.ok();
    let topics = received.lock().unwrap().clone();
    topics
}

#[tokio::test]
async fn sys_topics_enabled_publishes() {
    let config = base_config().with_sys_topics_interval(Duration::from_millis(100));
    let broker = TestBroker::start_with_config(config).await;

    let topics = collect_sys_topics(broker.address(), Duration::from_millis(400)).await;

    assert!(
        topics.iter().any(|t| t == "$SYS/broker/version"),
        "expected $SYS/broker/version, got {topics:?}"
    );
    assert!(
        topics.iter().any(|t| t == "$SYS/broker/uptime"),
        "expected $SYS/broker/uptime, got {topics:?}"
    );
}

#[tokio::test]
async fn sys_topics_interval_is_respected() {
    let interval = Duration::from_millis(300);
    let config = base_config().with_sys_topics_interval(interval);
    let broker = TestBroker::start_with_config(config).await;

    let client = MqttClient::new("sys-interval-sub");
    client.connect(broker.address()).await.expect("connect");

    let arrivals: Arc<Mutex<Vec<Instant>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&arrivals);
    client
        .subscribe("$SYS/broker/uptime", move |_msg| {
            sink.lock().unwrap().push(Instant::now());
        })
        .await
        .expect("subscribe");

    tokio::time::sleep(interval * 5).await;
    client.disconnect().await.ok();

    let times = arrivals.lock().unwrap().clone();
    assert!(
        times.len() >= 3,
        "expected at least 3 uptime republishes, got {}",
        times.len()
    );

    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    let steady = &gaps[1..];
    for gap in steady {
        assert!(
            *gap >= interval / 2 && *gap <= interval * 2,
            "republish gap {gap:?} not near configured interval {interval:?}"
        );
    }
}

#[tokio::test]
async fn sys_topics_disabled_publishes_nothing() {
    let config = base_config().with_sys_topics_enabled(false);
    let broker = TestBroker::start_with_config(config).await;

    let topics = collect_sys_topics(broker.address(), Duration::from_millis(400)).await;

    assert!(
        topics.is_empty(),
        "expected no $SYS topics when disabled, got {topics:?}"
    );
}
