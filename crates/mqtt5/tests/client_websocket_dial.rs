#![cfg(feature = "transport-websocket")]

use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);

async fn dials_for_one_failed_connect(scheme: &str) -> usize {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });

    let client = MqttClient::with_options(
        ConnectOptions::new(format!("dial-{scheme}")).with_automatic_reconnect(false),
    );
    let result = timeout(
        WAIT,
        client.connect(&format!("{scheme}://localhost:{port}/mqtt")),
    )
    .await
    .expect("connect finishes");
    assert!(result.is_err(), "connect to a closing server fails");
    accepted.load(Ordering::SeqCst)
}

#[tokio::test]
async fn ws_connect_dials_the_server_once() {
    assert_eq!(dials_for_one_failed_connect("ws").await, 1);
}

#[tokio::test]
async fn wss_connect_dials_the_server_once() {
    assert_eq!(dials_for_one_failed_connect("wss").await, 1);
}
