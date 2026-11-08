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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_bridge_knows_it_is_connected_after_an_outage() {
    let proxy = Proxy::start(remote_broker().await).await;
    let mut config = BridgeConfig::new("plain-outage", format!("127.0.0.1:{}", proxy.port))
        .add_topic("in/#", BridgeDirection::In, QoS::AtLeastOnce);
    config.retry_jitter = false;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(bridge.get_stats().await.connected);

    proxy.go_down();
    tokio::time::sleep(Duration::from_secs(6)).await;
    proxy.come_back().await;
    tokio::time::sleep(Duration::from_secs(20)).await;

    let stats = bridge.get_stats().await;
    assert!(stats.connected, "{:?}", stats.last_error);
    assert_eq!(proxy.connections.load(Ordering::SeqCst), 1);
    let _ = bridge.stop().await;
    task.abort();
}
