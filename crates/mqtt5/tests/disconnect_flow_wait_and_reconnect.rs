use mqtt5::{ConnectOptions, MqttClient};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CONNACK_RECEIVE_MAXIMUM_ONE: [u8; 8] = [0x20, 0x06, 0x00, 0x00, 0x03, 0x21, 0x00, 0x01];

#[tokio::test]
async fn disconnect_releases_a_flow_waiting_publish_and_a_reconnect_waiting_for_connack() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counting = Arc::clone(&accepted);
    let (drop_first, first_dropped) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut first, _) = listener.accept().await.unwrap();
        counting.fetch_add(1, Ordering::SeqCst);
        let mut buffer = [0u8; 4096];
        let _ = first.read(&mut buffer).await;
        first.write_all(&CONNACK_RECEIVE_MAXIMUM_ONE).await.unwrap();
        let _ = first_dropped.await;
        drop(first);
        let mut silent = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            counting.fetch_add(1, Ordering::SeqCst);
            silent.push(stream);
        }
    });

    let client = MqttClient::with_options(
        ConnectOptions::new("cancel-flow")
            .with_reconnect_delay(Duration::from_millis(50), Duration::from_millis(50))
            .with_connect_packet_timeout(Duration::from_secs(30)),
    );
    client.connect(&format!("mqtt://{addr}")).await.unwrap();

    let first_publisher = client.clone();
    let first =
        tokio::spawn(async move { first_publisher.publish_qos1("t/a", b"1".to_vec()).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second_publisher = client.clone();
    let second =
        tokio::spawn(async move { second_publisher.publish_qos1("t/b", b"2".to_vec()).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second_waiting = !second.is_finished();

    drop_first.send(()).unwrap();
    let started = Instant::now();
    while accepted.load(Ordering::SeqCst) < 2 && started.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let reconnecting = accepted.load(Ordering::SeqCst) >= 2;

    let started = Instant::now();
    let disconnected = tokio::time::timeout(Duration::from_secs(3), client.disconnect()).await;
    let took = started.elapsed();
    let first_done = tokio::time::timeout(Duration::from_secs(3), first).await;
    let second_done = tokio::time::timeout(Duration::from_secs(3), second).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let accepted_after = accepted.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        second_waiting,
        "second publish did not wait for flow control"
    );
    assert!(reconnecting, "no reconnection attempt reached the server");
    assert!(
        disconnected.is_ok(),
        "disconnect did not return in 3 s: {took:?}"
    );
    assert!(first_done.is_ok(), "first publish still waiting");
    assert!(second_done.is_ok(), "second publish still waiting");
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        accepted_after,
        "reconnection continued after disconnect"
    );
    assert!(!client.is_connected().await);
}
