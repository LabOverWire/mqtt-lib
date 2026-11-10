#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::{MqttClient, QoS, SubscribeOptions};
use mqtt5_protocol::packet::publish::PublishPacket;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn primary_broker() -> SocketAddr {
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

async fn black_hole_backup(publishes: Arc<AtomicUsize>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let publishes = Arc::clone(&publishes);
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                if stream
                    .write_all(&[0x20, 0x03, 0x00, 0x00, 0x00])
                    .await
                    .is_err()
                {
                    return;
                }
                loop {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if buf[..n].iter().any(|b| b >> 4 == 3) {
                                publishes.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                    }
                }
            });
        }
    });
    port
}

fn bring_up_primary(primary_port: u16, primary_broker: SocketAddr) {
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", primary_port)).unwrap();
    std_listener.set_nonblocking(true).unwrap();
    let listener = TcpListener::from_std(std_listener).unwrap();
    tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            tokio::spawn(async move {
                if let Ok(mut server) = tokio::net::TcpStream::connect(primary_broker).await {
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                }
            });
        }
    });
}

async fn inflight_message_across_failback(qos: QoS) -> (bool, usize) {
    let primary_broker = primary_broker().await;
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_port = closed.local_addr().unwrap().port();
    drop(closed);
    let publishes = Arc::new(AtomicUsize::new(0));
    let backup_port = black_hole_backup(Arc::clone(&publishes)).await;

    let received = Arc::new(Mutex::new(Vec::<String>::new()));
    let sub = MqttClient::new("inflight-sub");
    sub.connect(&format!("mqtt://{primary_broker}"))
        .await
        .unwrap();
    let seen = Arc::clone(&received);
    sub.subscribe_with_options(
        "out/#",
        SubscribeOptions {
            qos: QoS::ExactlyOnce,
            ..SubscribeOptions::default()
        },
        move |msg| {
            seen.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&msg.payload).to_string());
        },
    )
    .await
    .unwrap();

    let mut config = BridgeConfig::new("inflight", format!("127.0.0.1:{primary_port}")).add_topic(
        "out/#",
        BridgeDirection::Out,
        qos,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.first_retry_delay = Duration::from_millis(100);
    config.initial_reconnect_delay = Duration::from_millis(200);
    config.backup_brokers = vec![format!("127.0.0.1:{backup_port}")];
    config.primary_health_check_interval = Duration::from_millis(600);
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    for _ in 0..200 {
        if bridge.get_stats().await.connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(bridge.get_stats().await.connected);
    assert!(!bridge.get_stats().await.on_primary);

    let packet = PublishPacket::new("out/x", b"held".to_vec(), qos);
    bridge.forward_message(&packet);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let sent_to_backup = publishes.load(Ordering::SeqCst);

    bring_up_primary(primary_port, primary_broker);
    let mut on_primary = false;
    for _ in 0..100 {
        let stats = bridge.get_stats().await;
        if stats.connected && stats.on_primary {
            on_primary = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(on_primary, "failed back");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let arrived = received.lock().unwrap().iter().any(|p| p == "held");
    let _ = bridge.stop().await;
    task.abort();
    (arrived, sent_to_backup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_qos1_inflight_on_backup_arrives_at_primary_after_failback() {
    let (arrived, sent) = inflight_message_across_failback(QoS::AtLeastOnce).await;
    assert!(sent >= 1, "the PUBLISH reached the backup");
    assert!(arrived, "QoS 1 in-flight message lost across failback");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos2_inflight_on_backup_arrives_at_primary_after_failback() {
    let (arrived, sent) = inflight_message_across_failback(QoS::ExactlyOnce).await;
    assert!(sent >= 1, "the PUBLISH reached the backup");
    assert!(arrived, "QoS 2 in-flight message lost across failback");
}
