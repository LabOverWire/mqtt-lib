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

async fn broker_with_receive_maximum_one() -> SocketAddr {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(StorageConfig::new().with_persistence(false))
        .with_server_receive_maximum(1);
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
                let mut first = true;
                let mut buf = vec![0u8; 4096];
                loop {
                    match server_read.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if !first {
                                tokio::time::sleep(delay).await;
                            }
                            first = false;
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

async fn missing_after_forwarding(total: usize) -> Vec<usize> {
    let remote = broker_with_receive_maximum_one().await;
    let port = slow_downstream_proxy(remote, Duration::from_secs(2)).await;
    let received = Arc::new(Mutex::new(HashSet::new()));
    let sub = MqttClient::new("quota-wait-sub");
    sub.connect(&format!("mqtt://{remote}")).await.unwrap();
    let seen = Arc::clone(&received);
    sub.subscribe("out/#", move |msg| {
        seen.lock()
            .unwrap()
            .insert(String::from_utf8_lossy(&msg.payload).to_string());
    })
    .await
    .unwrap();

    let mut config = BridgeConfig::new("quota-wait", format!("127.0.0.1:{port}")).add_topic(
        "out/#",
        BridgeDirection::Out,
        QoS::AtLeastOnce,
    );
    config.retry_jitter = false;
    config.connection_retries = 1;
    config.fallback_tcp = false;
    let bridge = Arc::new(BridgeConnection::new(config, Arc::new(MessageRouter::new())).unwrap());
    bridge.start();
    let running = Arc::clone(&bridge);
    let task = tokio::spawn(async move { running.run().await });
    for _ in 0..200 {
        if bridge.get_stats().await.connected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(bridge.get_stats().await.connected);
    for i in 0..total {
        let packet = PublishPacket::new("out/x", format!("{i}").into_bytes(), QoS::AtLeastOnce);
        bridge.forward_message(&packet).await.unwrap();
    }
    let deadline = Duration::from_secs(2 * total as u64 + 20);
    let started = tokio::time::Instant::now();
    while started.elapsed() < deadline && received.lock().unwrap().len() < total {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let _ = bridge.stop().await;
    task.abort();
    (0..total)
        .filter(|i| !received.lock().unwrap().contains(&format!("{i}")))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn control_messages_waiting_less_than_30s_for_quota_all_arrive() {
    let missing = missing_after_forwarding(8).await;
    assert!(missing.is_empty(), "missing: {missing:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn messages_waiting_more_than_30s_for_quota_all_arrive() {
    let missing = missing_after_forwarding(22).await;
    assert!(
        missing.is_empty(),
        "QoS 1 messages lost while waiting for the remote's quota: {missing:?}"
    );
}
