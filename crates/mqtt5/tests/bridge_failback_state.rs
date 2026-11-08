#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::QoS;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
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
    listening: Mutex<Option<JoinHandle<()>>>,
}

impl Proxy {
    async fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Self {
            port: listener.local_addr().unwrap().port(),
            upstream,
            connections: Arc::new(AtomicUsize::new(0)),
            listening: Mutex::new(None),
        };
        proxy.listen(listener);
        proxy
    }

    fn listen(&self, listener: TcpListener) {
        let (upstream, connections) = (self.upstream, Arc::clone(&self.connections));
        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let connections = Arc::clone(&connections);
                tokio::spawn(async move {
                    connections.fetch_add(1, Ordering::SeqCst);
                    if let Ok(mut server) = tokio::net::TcpStream::connect(upstream).await {
                        let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                    }
                    connections.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        *self.listening.lock().unwrap() = Some(task);
    }

    fn stop_accepting(&self) {
        if let Some(task) = self.listening.lock().unwrap().take() {
            task.abort();
        }
    }

    async fn accept_again(&self) {
        self.listen(TcpListener::bind(("127.0.0.1", self.port)).await.unwrap());
    }
}

fn refusing_primary() -> u16 {
    let refusing = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    refusing.set_nonblocking(true).unwrap();
    let port = refusing.local_addr().unwrap().port();
    let refusing = TcpListener::from_std(refusing).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = refusing.accept().await {
            drop(stream);
        }
    });
    port
}

fn config_for(name: &str, primary: u16, backup: u16) -> BridgeConfig {
    let mut config = BridgeConfig::new(name, format!("127.0.0.1:{primary}")).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(100);
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.max_reconnect_delay = Duration::from_secs(60);
    config.backup_brokers = vec![format!("127.0.0.1:{backup}")];
    config.primary_health_check_interval = Duration::from_secs(1);
    config
}

async fn wait_for(connections: &AtomicUsize, count: usize) -> bool {
    for _ in 0..200 {
        if connections.load(Ordering::SeqCst) == count {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

async fn time_to_next_failback(bridge: &BridgeConnection, before: u64) -> Duration {
    let started = tokio::time::Instant::now();
    while bridge.get_stats().await.failback_attempts == before {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "no failback probe"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    started.elapsed()
}

fn spawn_run(bridge: &Arc<BridgeConnection>) -> JoinHandle<()> {
    let running = Arc::clone(bridge);
    tokio::spawn(async move {
        let _ = running.run().await;
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn running_again_after_a_failback_that_failed_outright_is_not_a_failed_failback() {
    let primary_port = refusing_primary();
    let backup = Proxy::start(remote_broker().await).await;
    let mut config = config_for("stale-flag", primary_port, backup.port);
    config.max_reconnect_attempts = Some(1);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let task = spawn_run(&bridge);
    assert!(wait_for(&backup.connections, 1).await, "reached backup");
    backup.stop_accepting();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("run() ends when the failback's connect fails outright")
        .unwrap();
    let failbacks = bridge.get_stats().await.failback_attempts;
    assert_eq!(failbacks, 1);

    backup.accept_again().await;
    let task = spawn_run(&bridge);
    assert!(
        wait_for(&backup.connections, 1).await,
        "reached backup again"
    );
    let waited = time_to_next_failback(&bridge, failbacks).await;
    let _ = bridge.stop().await;
    task.abort();
    assert!(
        waited < Duration::from_millis(1600),
        "running again was counted as a failed failback: first probe after {waited:?}, interval 1s"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_starts_the_failback_wait_from_the_configured_interval() {
    let primary_port = refusing_primary();
    let backup = Proxy::start(remote_broker().await).await;
    let bridge = Arc::new(
        BridgeConnection::new(
            config_for("stale-count", primary_port, backup.port),
            Arc::new(MessageRouter::new()),
        )
        .unwrap(),
    );
    bridge.start();
    let task = spawn_run(&bridge);
    assert!(wait_for(&backup.connections, 1).await, "reached backup");
    for failbacks in 0..3 {
        let waited = time_to_next_failback(&bridge, failbacks).await;
        assert!(
            waited >= Duration::from_millis(900 << failbacks),
            "{waited:?}"
        );
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(wait_for(&backup.connections, 1).await, "back on backup");
    assert!(bridge.get_stats().await.connected);
    bridge.stop().await.unwrap();
    task.abort();
    let failbacks = bridge.get_stats().await.failback_attempts;

    bridge.start();
    let task = spawn_run(&bridge);
    assert!(
        wait_for(&backup.connections, 1).await,
        "reached backup again"
    );
    let waited = time_to_next_failback(&bridge, failbacks).await;
    let _ = bridge.stop().await;
    task.abort();
    assert!(
        waited < Duration::from_millis(1600),
        "stop/start kept the failback backoff: first probe after {waited:?}, interval 1s"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_right_after_a_probe_does_not_leave_a_failback_pending() {
    let primary_port = refusing_primary();
    let backup_broker = remote_broker().await;
    let mut config = BridgeConfig::new("stop-race", format!("127.0.0.1:{primary_port}")).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(100);
    config.max_reconnect_delay = Duration::from_secs(60);
    config.backup_brokers = vec![backup_broker.to_string()];
    config.primary_health_check_interval = Duration::from_millis(400);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());

    let mut waits = Vec::new();
    for _ in 0..40 {
        bridge.start();
        let running = Arc::clone(&bridge);
        let task = tokio::spawn(async move { running.run().await });
        loop {
            let stats = bridge.get_stats().await;
            if stats.connected && !stats.on_primary {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let before = bridge.get_stats().await.failback_attempts;
        let reached = tokio::time::Instant::now();
        while bridge.get_stats().await.failback_attempts == before {
            tokio::task::yield_now().await;
        }
        waits.push(reached.elapsed().as_millis());
        bridge.stop().await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }
    let doubled: Vec<_> = waits.iter().filter(|&&ms| ms >= 650).collect();
    assert!(
        doubled.is_empty(),
        "first probe waits after restart (ms): {waits:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_reports_exceeding_max_reconnect_attempts_as_an_error() {
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let mut config = BridgeConfig::new("run-error", format!("127.0.0.1:{port}")).add_topic(
        "in/#",
        BridgeDirection::In,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.max_reconnect_attempts = Some(1);
    let bridge = BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap();
    bridge.start();
    let result = tokio::time::timeout(Duration::from_secs(10), bridge.run())
        .await
        .expect("run ends");
    assert!(result.is_err(), "run() returned {result:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tiny_interval_keeps_doubling_up_to_max_reconnect_delay() {
    let primary_port = refusing_primary();
    let backup = remote_broker().await;
    let mut config = BridgeConfig::new("tiny-interval", format!("127.0.0.1:{primary_port}"))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    config.backup_brokers = vec![backup.to_string()];
    config.primary_health_check_interval = Duration::from_micros(1);
    config.max_reconnect_delay = Duration::from_secs(300);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_secs(4)).await;
    let early = bridge.get_stats().await.failback_attempts;
    tokio::time::sleep(Duration::from_secs(4)).await;
    let late = bridge.get_stats().await.failback_attempts;
    let _ = bridge.stop().await;
    task.abort();
    assert!(
        late - early <= 3,
        "failback period stopped doubling below max_reconnect_delay: {} failbacks between 4s and 8s ({early} -> {late})",
        late - early
    );
}
