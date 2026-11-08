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
            cut: Arc::new(Notify::new()),
            listening: Mutex::new(None),
        };
        proxy.listen(listener);
        proxy
    }

    fn listen(&self, listener: TcpListener) {
        let (upstream, connections, cut) = (
            self.upstream,
            Arc::clone(&self.connections),
            Arc::clone(&self.cut),
        );
        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bridge_fails_back_to_the_primary_and_keeps_running() {
    let primary = Proxy::start(remote_broker().await).await;
    primary.go_down();
    let backup = Proxy::start(remote_broker().await).await;
    let mut config = BridgeConfig::new("failback", format!("127.0.0.1:{}", primary.port))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.backup_brokers = vec![format!("127.0.0.1:{}", backup.port)];
    config.primary_health_check_interval = Duration::from_millis(300);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    assert!(
        wait_for(&backup.connections, 1).await,
        "the bridge reached the backup"
    );

    primary.come_back().await;
    assert!(
        wait_for(&primary.connections, 1).await,
        "the bridge failed back to the primary"
    );
    assert!(
        wait_for(&backup.connections, 0).await,
        "the bridge left the backup"
    );
    assert!(!task.is_finished());
    let mut on_primary = false;
    for _ in 0..100 {
        let stats = bridge.get_stats().await;
        if stats.connected && stats.on_primary {
            on_primary = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(on_primary, "the bridge is connected to the primary");

    primary.go_down();
    assert!(
        wait_for(&backup.connections, 1).await,
        "the bridge reconnected to the backup"
    );
    assert!(!task.is_finished());
    let _ = bridge.stop().await;
    task.abort();
}
