#![cfg(feature = "broker")]
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::MqttClient;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

async fn start_broker() -> (
    u16,
    mqtt5::broker::BrokerShutdownHandle,
    tokio::task::JoinHandle<mqtt5::error::Result<()>>,
) {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(mqtt5::broker::config::StorageConfig::default().with_persistence(false));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.local_addr().unwrap().port();
    let handle = broker.shutdown_handle();
    let task = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (port, handle, task)
}

#[tokio::test]
async fn graceful_shutdown_stops_accept_loop_and_returns() {
    let (port, shutdown, task) = start_broker().await;
    let addr = format!("127.0.0.1:{port}");

    let received = Arc::new(AtomicU32::new(0));
    let recv_clone = Arc::clone(&received);

    let sub = MqttClient::new("sub");
    sub.connect(&format!("mqtt://{addr}")).await.unwrap();
    sub.subscribe("test/topic", move |_msg| {
        recv_clone.fetch_add(1, Ordering::SeqCst);
    })
    .await
    .unwrap();

    let pubc = MqttClient::new("pub");
    pubc.connect(&format!("mqtt://{addr}")).await.unwrap();
    pubc.publish("test/topic", b"hello").await.unwrap();

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        received.load(Ordering::SeqCst),
        1,
        "message should be delivered before shutdown"
    );

    assert!(
        TcpStream::connect(&addr).await.is_ok(),
        "listener should accept before shutdown"
    );

    shutdown.shutdown();

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("run() did not return after shutdown signal")
        .expect("run task panicked");
    assert!(result.is_ok(), "run() returned error: {result:?}");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut refused = false;
    for _ in 0..20 {
        if let Ok(Ok(stream)) =
            tokio::time::timeout(Duration::from_millis(100), TcpStream::connect(&addr)).await
        {
            drop(stream);
            tokio::time::sleep(Duration::from_millis(50)).await;
        } else {
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "listener should stop accepting connections after graceful shutdown"
    );
}

#[tokio::test]
async fn double_run_is_rejected() {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_storage(mqtt5::broker::config::StorageConfig::default().with_persistence(false));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let handle = broker.shutdown_handle();

    let first = tokio::spawn(async move {
        let r = broker.run().await;
        (broker, r)
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.shutdown();
    let (mut broker, r1) = tokio::time::timeout(Duration::from_secs(5), first)
        .await
        .expect("first run did not return")
        .expect("first run panicked");
    assert!(r1.is_ok());

    let r2 = broker.run().await;
    assert!(
        r2.is_err(),
        "second run() should be rejected once listeners are drained"
    );
}
