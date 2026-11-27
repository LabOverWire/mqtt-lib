#![cfg(feature = "broker")]

use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, Message, MqttClient, WillMessage};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn broker() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    (addr, handle)
}

async fn answer_delaying_proxy(
    upstream: SocketAddr,
    delay: Duration,
) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let forwarded = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&forwarded);
    tokio::spawn(async move {
        while let Ok((downstream, _)) = listener.accept().await {
            let counting = Arc::clone(&counting);
            tokio::spawn(async move {
                let Ok(up) = TcpStream::connect(upstream).await else {
                    return;
                };
                let (mut down_read, mut down_write) = downstream.into_split();
                let (mut up_read, mut up_write) = up.into_split();
                let to_broker = tokio::spawn(async move {
                    let mut buffer = [0u8; 4096];
                    while let Ok(read) = down_read.read(&mut buffer).await {
                        if read == 0 {
                            break;
                        }
                        counting.fetch_add(read, Ordering::SeqCst);
                        if up_write.write_all(&buffer[..read]).await.is_err() {
                            break;
                        }
                    }
                    let _ = up_write.shutdown().await;
                });
                tokio::time::sleep(delay).await;
                let _ = tokio::io::copy(&mut up_read, &mut down_write).await;
                let _ = to_broker.await;
            });
        }
    });
    (addr, forwarded)
}

async fn will_watcher(addr: SocketAddr, topic: &str) -> (MqttClient, Arc<AtomicUsize>) {
    let watcher = MqttClient::new(format!("cancel-watcher-{}", topic.replace('/', "-")));
    watcher.connect(&format!("mqtt://{addr}")).await.unwrap();
    let wills = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&wills);
    watcher
        .subscribe(topic, move |_: Message| {
            counting.fetch_add(1, Ordering::SeqCst);
        })
        .await
        .unwrap();
    (watcher, wills)
}

fn with_will(client_id: &str, topic: &str) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_will(WillMessage::new(topic, b"gone".to_vec()))
        .with_automatic_reconnect(false)
}

async fn wait_forwarded(forwarded: &AtomicUsize) {
    while forwarded.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[tokio::test]
async fn abnormal_disconnect_while_connack_is_in_flight_publishes_the_will() {
    let (broker_addr, broker) = broker().await;
    let (watcher, wills) = will_watcher(broker_addr, "cancel/will/a").await;
    let (proxy_addr, forwarded) =
        answer_delaying_proxy(broker_addr, Duration::from_millis(1500)).await;
    let client = MqttClient::with_options(with_will("cancel-subject-a", "cancel/will/a"));
    let connecting = client.clone();
    let url = format!("mqtt://{proxy_addr}");
    let connect = tokio::spawn(async move { connecting.connect(&url).await });
    wait_forwarded(&forwarded).await;
    let started = Instant::now();
    let disconnected = client.disconnect_abnormally().await;
    let took = started.elapsed();
    let connected = connect.await.unwrap();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let received = wills.load(Ordering::SeqCst);
    watcher.disconnect().await.ok();
    broker.abort();
    assert_eq!(
        received, 1,
        "abnormal disconnect discarded the will: disconnect_abnormally returned {disconnected:?} after {took:?}, connect returned {connected:?}"
    );
}
