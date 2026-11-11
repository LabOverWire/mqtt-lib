#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeConnection, BridgeDirection};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::{BrokerConfig, MqttBroker, StorageConfig};
use mqtt5::time::Duration;
use mqtt5::{MqttClient, QoS};
use mqtt5_protocol::packet::publish::PublishPacket;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn broker_with(receive_maximum: Option<u16>) -> SocketAddr {
    let mut config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::new().with_persistence(false));
    if let Some(max) = receive_maximum {
        config = config.with_server_receive_maximum(max);
    }
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    addr
}

async fn slow_downstream_proxy(upstream: SocketAddr, delay: Duration) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            tokio::spawn(async move {
                let Ok(server) = tokio::net::TcpStream::connect(upstream).await else {
                    return;
                };
                let (mut client_read, mut client_write) = client.into_split();
                let (mut server_read, mut server_write) = server.into_split();
                let up = tokio::spawn(async move {
                    let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
                });
                let mut buf = vec![0u8; 16];
                loop {
                    match server_read.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            tokio::time::sleep(delay).await;
                            if client_write.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
                up.abort();
            });
        }
    });
    port
}

async fn subscribe_all(
    brokers: &[SocketAddr],
    received: &Arc<Mutex<HashSet<String>>>,
) -> Vec<MqttClient> {
    let mut subscribers = Vec::new();
    for (index, addr) in brokers.iter().enumerate() {
        let sub = MqttClient::new(format!("quota-sub-{index}"));
        sub.connect(&format!("mqtt://{addr}")).await.unwrap();
        let seen = Arc::clone(received);
        sub.subscribe("out/#", move |msg| {
            seen.lock()
                .unwrap()
                .insert(String::from_utf8_lossy(&msg.payload).to_string());
        })
        .await
        .unwrap();
        subscribers.push(sub);
    }
    subscribers
}

async fn forwarded_across(fail_back: bool, qos: QoS) -> Vec<usize> {
    let primary_broker = broker_with(None).await;
    let primary_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let primary_port = primary_listener.local_addr().unwrap().port();
    drop(primary_listener);
    let backup_broker = broker_with(Some(1)).await;
    let backup_port = slow_downstream_proxy(backup_broker, Duration::from_millis(20)).await;

    let received = Arc::new(Mutex::new(HashSet::new()));
    let subscribers = subscribe_all(&[primary_broker, backup_broker], &received).await;

    let mut config = BridgeConfig::new("quota-failback", format!("127.0.0.1:{primary_port}"))
        .add_topic("out/#", BridgeDirection::Out, qos);
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
    for _ in 0..100 {
        if bridge.get_stats().await.connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!bridge.get_stats().await.on_primary);

    if fail_back {
        let listener = TcpListener::bind(("127.0.0.1", primary_port))
            .await
            .unwrap();
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

    let total = 60usize;
    for i in 0..total {
        let packet = PublishPacket::new("out/x", format!("{i}").into_bytes(), qos);
        bridge.forward_message(&packet);
    }
    tokio::time::sleep(Duration::from_secs(8)).await;
    let stats = bridge.get_stats().await;
    if fail_back {
        assert!(stats.on_primary, "the bridge failed back: {stats:?}");
    }
    let missing: Vec<usize> = (0..total)
        .filter(|i| !received.lock().unwrap().contains(&format!("{i}")))
        .collect();
    let payload_bytes: usize = (0..total).map(|i| format!("{i}").len()).sum();
    if missing.is_empty() {
        assert_eq!(stats.messages_sent, total as u64, "{stats:?}");
        assert_eq!(stats.bytes_sent, payload_bytes as u64, "{stats:?}");
    }
    let _ = bridge.stop().await;
    task.abort();
    drop(subscribers);
    missing
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_a_failback_every_qos1_message_waiting_for_quota_arrives() {
    let missing = forwarded_across(false, QoS::AtLeastOnce).await;
    assert!(missing.is_empty(), "missing without failback: {missing:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos1_messages_waiting_for_quota_survive_a_failback() {
    let missing = forwarded_across(true, QoS::AtLeastOnce).await;
    assert!(missing.is_empty(), "lost across failback: {missing:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qos2_messages_waiting_for_quota_survive_a_failback() {
    let missing = forwarded_across(true, QoS::ExactlyOnce).await;
    assert!(missing.is_empty(), "lost across failback: {missing:?}");
}
