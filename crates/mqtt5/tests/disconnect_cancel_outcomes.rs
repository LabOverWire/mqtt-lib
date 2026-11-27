use mqtt5::{ConnectOptions, MqttClient};
use std::future::Future;
use std::task::Poll;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;

#[tokio::test]
async fn withdrawn_graceful_disconnect_during_connack_wait() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let client = MqttClient::with_options(
        ConnectOptions::new("cancel-withdrawn").with_automatic_reconnect(false),
    );
    let connecting = client.clone();
    let url = format!("mqtt://{addr}");
    let connect = tokio::spawn(async move { connecting.connect(&url).await });
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut buffer = [0u8; 1024];
    let first = stream.read(&mut buffer).await.unwrap();
    assert!(first > 0);

    let mut disconnect = Box::pin(client.disconnect());
    let ready =
        std::future::poll_fn(|cx| Poll::Ready(disconnect.as_mut().poll(cx).is_ready())).await;
    drop(disconnect);

    let outcome = tokio::time::timeout(Duration::from_secs(2), connect).await;
    let mut rest = Vec::new();
    if let Ok(Ok(read)) =
        tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buffer)).await
    {
        rest.extend_from_slice(&buffer[..read]);
    }
    tracing::info!(ready, ?outcome, ?rest, "observed");
    let finished = outcome.is_ok();
    let sent_disconnect = rest.first() == Some(&0xE0);
    assert!(
        !finished,
        "a dropped disconnect future cancelled the connect: ready_at_first_poll={ready} outcome={outcome:?} sent_disconnect={sent_disconnect}"
    );
}

#[tokio::test]
async fn cancelled_connect_error_variants_match_changelog() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let client = MqttClient::with_options(
        ConnectOptions::new("cancel-variants").with_automatic_reconnect(false),
    );
    let connecting = client.clone();
    let url = format!("mqtt://{addr}");
    let connect = tokio::spawn(async move { connecting.connect(&url).await });
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut buffer = [0u8; 1024];
    let first = stream.read(&mut buffer).await.unwrap();
    assert!(first > 0);
    let disconnected = tokio::time::timeout(Duration::from_secs(2), client.disconnect())
        .await
        .unwrap();
    let connected = connect.await.unwrap();
    let mut rest = Vec::new();
    if let Ok(Ok(read)) =
        tokio::time::timeout(Duration::from_millis(500), stream.read(&mut buffer)).await
    {
        rest.extend_from_slice(&buffer[..read]);
    }
    assert!(
        matches!(disconnected, Err(mqtt5::MqttError::NotConnected)),
        "disconnect returned {disconnected:?}"
    );
    assert!(
        matches!(connected, Err(mqtt5::MqttError::ConnectionError(_))),
        "connect returned {connected:?}"
    );
    assert_eq!(
        rest.first(),
        Some(&0xE0),
        "no DISCONNECT after CONNECT: {rest:?}"
    );
    assert!(!client.is_connected().await);
}
