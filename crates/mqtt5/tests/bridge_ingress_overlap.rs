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

fn overlapping_bridge(remote: SocketAddr) -> BridgeConfig {
    let mut config = BridgeConfig::new("overlap", remote.to_string())
        .add_topic("sensors/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("sensors/+/temp", BridgeDirection::In, QoS::AtLeastOnce);
    config.topics[0].local_prefix = Some("a/".to_string());
    config.topics[1].local_prefix = Some("b/".to_string());
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.first_retry_delay = Duration::from_millis(200);
    config.max_reconnect_delay = Duration::from_millis(500);
    config
}

async fn subscribe_all(client: &MqttClient, filter: &str) -> Arc<Mutex<Vec<String>>> {
    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    client
        .subscribe(filter, move |msg| {
            sink.lock().unwrap().push(msg.topic.clone());
        })
        .await
        .unwrap();
    received
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn overlapping_incoming_mappings_route_one_copy_each() {
    let remote = start_broker(broker_config(free_port())).await;

    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![overlapping_bridge(remote.addr)];
    let local = start_broker(local_config).await;

    let local_client = MqttClient::new("local-subscriber");
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "#").await;

    let remote_client = MqttClient::new("remote-publisher");
    remote_client
        .connect(&format!("mqtt://{}", remote.addr))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    remote_client
        .publish_qos1("sensors/x/temp", b"21")
        .await
        .unwrap();

    assert!(
        wait_until(Duration::from_secs(5), || received.lock().unwrap().len()
            >= 2)
        .await,
        "both mappings deliver a local copy"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut topics = received.lock().unwrap().clone();
    topics.sort();
    assert_eq!(
        topics,
        vec![
            "a/sensors/x/temp".to_string(),
            "b/sensors/x/temp".to_string()
        ],
        "exactly one copy per mapping, none duplicated across overlapping filters"
    );

    remote_client.disconnect().await.unwrap();
    local_client.disconnect().await.unwrap();
    local.stop().await;
    remote.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bridge_keeps_delivering_across_remote_restarts() {
    let remote_bind = free_port();
    let mut remote = start_broker(broker_config(remote_bind)).await;

    let mut local_config = broker_config(free_port());
    local_config.bridges = vec![overlapping_bridge(remote.addr)];
    let local = start_broker(local_config).await;

    let local_client = MqttClient::new("local-subscriber");
    local_client
        .connect(&format!("mqtt://{}", local.addr))
        .await
        .unwrap();
    let received = subscribe_all(&local_client, "a/#").await;

    for round in 0..3 {
        if round > 0 {
            remote.stop().await;
            remote = start_broker(broker_config(remote_bind)).await;
        }
        let publisher = MqttClient::new(format!("remote-publisher-{round}"));
        publisher
            .connect(&format!("mqtt://{}", remote.addr))
            .await
            .unwrap();

        let before = received.lock().unwrap().len();
        let mut delivered = false;
        for _ in 0..100 {
            publisher
                .publish_qos1("sensors/y/temp", format!("{round}").into_bytes())
                .await
                .unwrap();
            if wait_until(Duration::from_millis(200), || {
                received.lock().unwrap().len() > before
            })
            .await
            {
                delivered = true;
                break;
            }
        }
        assert!(delivered, "delivery resumes after remote restart {round}");
        publisher.disconnect().await.unwrap();
    }

    local_client.disconnect().await.unwrap();
    local.stop().await;
    remote.stop().await;
}
