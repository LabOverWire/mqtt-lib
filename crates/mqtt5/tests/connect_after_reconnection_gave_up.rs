use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, ConnectionEvent, MqttClient};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn refused_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn closing_listener() -> (u16, Arc<AtomicUsize>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    (port, accepted)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connect_made_when_reconnection_gives_up_does_not_restart_it() {
    let (port, accepted) = closing_listener().await;
    let client = MqttClient::with_options(
        ConnectOptions::new("after-giving-up")
            .with_max_reconnect_attempts(2)
            .with_reconnect_delay(Duration::from_millis(200), Duration::from_millis(200)),
    );
    let gave_up = Arc::new(AtomicUsize::new(0));
    let handler_client = client.clone();
    let counted = Arc::clone(&gave_up);
    client
        .on_connection_event(move |event| {
            if let ConnectionEvent::ReconnectFailed { .. } = event {
                if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                    let client = handler_client.clone();
                    let _ = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async move {
                            client.connect(&format!("mqtt://127.0.0.1:{port}")).await
                        })
                    });
                }
            }
        })
        .await
        .unwrap();
    client
        .connect(&format!("mqtt://127.0.0.1:{}", refused_port()))
        .await
        .expect_err("nothing listens there");
    tokio::time::sleep(Duration::from_secs(6)).await;
    assert_eq!(gave_up.load(Ordering::SeqCst), 1);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}
