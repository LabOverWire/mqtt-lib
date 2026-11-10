#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::{MqttClient, QoS};
use mqtt5_protocol::packet::publish::PublishPacket;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

async fn remote_broker() -> SocketAddr {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::new().with_persistence(false));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    addr
}

struct Proxy {
    port: u16,
    upstream: SocketAddr,
    connections: Arc<AtomicUsize>,
    total: Arc<AtomicUsize>,
    cut: Arc<Notify>,
    listening: Mutex<Option<JoinHandle<()>>>,
}

impl Proxy {
    async fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Self {
            port: listener.local_addr().unwrap().port(),
            upstream,
            connections: Arc::new(AtomicUsize::new(0)),
            total: Arc::new(AtomicUsize::new(0)),
            cut: Arc::new(Notify::new()),
            listening: Mutex::new(None),
        };
        proxy.listen(listener);
        proxy
    }

    fn listen(&self, listener: TcpListener) {
        let (upstream, connections, total, cut) = (
            self.upstream,
            Arc::clone(&self.connections),
            Arc::clone(&self.total),
            Arc::clone(&self.cut),
        );
        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                total.fetch_add(1, Ordering::SeqCst);
                let (connections, cut) = (Arc::clone(&connections), Arc::clone(&cut));
                tokio::spawn(async move {
                    let cut = cut.notified();
                    tokio::pin!(cut);
                    cut.as_mut().enable();
                    connections.fetch_add(1, Ordering::SeqCst);
                    let Ok(mut server) = tokio::net::TcpStream::connect(upstream).await else {
                        connections.fetch_sub(1, Ordering::SeqCst);
                        return;
                    };
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut client, &mut server) => {}
                        () = &mut cut => {}
                    }
                    connections.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        *self.listening.lock().unwrap() = Some(task);
    }

    fn go_down(&self) {
        if let Some(task) = self.listening.lock().unwrap().take() {
            task.abort();
        }
        self.cut.notify_waiters();
    }

    async fn come_back(&self) {
        self.listen(TcpListener::bind(("127.0.0.1", self.port)).await.unwrap());
    }
}

async fn wait_for(connections: &AtomicUsize, count: usize) -> bool {
    for _ in 0..100 {
        if connections.load(Ordering::SeqCst) == count {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn config_for(name: &str, primary: u16, backup: u16) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, format!("127.0.0.1:{primary}"))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce)
        .add_topic("out/#", BridgeDirection::Out, QoS::AtLeastOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(100);
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.backup_brokers = vec![format!("127.0.0.1:{backup}")];
    config.primary_health_check_interval = Duration::from_millis(300);
    config
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_failback_the_bridge_receives_from_the_primary() {
    let primary_broker = remote_broker().await;
    let primary = Proxy::start(primary_broker).await;
    primary.go_down();
    let backup_broker = remote_broker().await;
    let backup = Proxy::start(backup_broker).await;
    let bridge = Arc::new(
        BridgeConnection::new(
            config_for("fb-in", primary.port, backup.port),
            Arc::new(MessageRouter::new()),
        )
        .unwrap(),
    );
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    assert!(wait_for(&backup.connections, 1).await, "reached backup");
    tokio::time::sleep(Duration::from_millis(500)).await;

    let backup_pub = MqttClient::new("backup-pub");
    backup_pub
        .connect(&format!("mqtt://{backup_broker}"))
        .await
        .unwrap();
    backup_pub
        .publish_qos1("in/a", b"on-backup".to_vec())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let on_backup = bridge.get_stats().await.messages_received;

    primary.come_back().await;
    assert!(wait_for(&primary.connections, 1).await, "failed back");
    assert!(wait_for(&backup.connections, 0).await, "left backup");
    tokio::time::sleep(Duration::from_secs(1)).await;

    let primary_pub = MqttClient::new("primary-pub");
    primary_pub
        .connect(&format!("mqtt://{primary_broker}"))
        .await
        .unwrap();
    primary_pub
        .publish_qos1("in/b", b"on-primary".to_vec())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    let stats = bridge.get_stats().await;
    assert_eq!(on_backup, 1, "received on backup");
    assert_eq!(
        stats.messages_received, 2,
        "received on primary after failback"
    );
    let _ = bridge.stop().await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failbacks_to_a_primary_that_refuses_mqtt_back_off() {
    let refusing = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_port = refusing.local_addr().unwrap().port();
    let refused = Arc::new(AtomicUsize::new(0));
    let refused_count = Arc::clone(&refused);
    tokio::spawn(async move {
        while let Ok((stream, _)) = refusing.accept().await {
            refused_count.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    let backup = Proxy::start(remote_broker().await).await;
    let bridge = Arc::new(
        BridgeConnection::new(
            config_for("fb-flap", primary_port, backup.port),
            Arc::new(MessageRouter::new()),
        )
        .unwrap(),
    );
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    assert!(wait_for(&backup.connections, 1).await, "reached backup");
    tokio::time::sleep(Duration::from_secs(5)).await;
    let stats = bridge.get_stats().await;
    assert!(
        stats.failback_attempts <= 5,
        "failbacks didn't back off: {}",
        stats.failback_attempts
    );
    assert!(backup.total.load(Ordering::SeqCst) <= 6);
    assert!(!task.is_finished());
    let _ = bridge.stop().await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos1_messages_forwarded_across_a_failback_all_arrive() {
    let primary_broker = remote_broker().await;
    let primary = Proxy::start(primary_broker).await;
    primary.go_down();
    let backup_broker = remote_broker().await;
    let backup = Proxy::start(backup_broker).await;

    let received = Arc::new(Mutex::new(HashSet::new()));
    let mut subscribers = Vec::new();
    for (name, addr) in [("sub-p", primary_broker), ("sub-b", backup_broker)] {
        let sub = MqttClient::new(name);
        sub.connect(&format!("mqtt://{addr}")).await.unwrap();
        let seen = Arc::clone(&received);
        sub.subscribe("out/#", move |msg| {
            seen.lock()
                .unwrap()
                .insert(String::from_utf8_lossy(&msg.payload).to_string());
        })
        .await
        .unwrap();
        subscribers.push(sub);
    }

    let mut config = config_for("fb-out", primary.port, backup.port);
    config.primary_health_check_interval = Duration::from_secs(2);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    assert!(wait_for(&backup.connections, 1).await, "reached backup");
    tokio::time::sleep(Duration::from_millis(500)).await;

    primary.come_back().await;
    let total = 1500usize;
    for i in 0..total {
        let packet = PublishPacket::new("out/x", format!("{i}").into_bytes(), QoS::AtLeastOnce);
        bridge.forward_message(&packet);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(wait_for(&primary.connections, 1).await, "failed back");
    tokio::time::sleep(Duration::from_secs(3)).await;
    let missing: Vec<usize> = (0..total)
        .filter(|i| !received.lock().unwrap().contains(&format!("{i}")))
        .collect();
    assert!(missing.is_empty(), "QoS 1 messages lost across failback");
    let _ = bridge.stop().await;
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_failback_waits_a_full_interval() {
    let refusing = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_port = refusing.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = refusing.accept().await {
            drop(stream);
        }
    });
    let backup = Proxy::start(remote_broker().await).await;
    let mut config = BridgeConfig::new("fb-default", format!("127.0.0.1:{primary_port}"))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.backup_brokers = vec![format!("127.0.0.1:{}", backup.port)];
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_secs(15)).await;
    let stats = bridge.get_stats().await;
    assert_eq!(backup.total.load(Ordering::SeqCst), 1);
    assert_eq!(stats.failback_attempts, 0);
    let _ = bridge.stop().await;
    task.abort();
}
