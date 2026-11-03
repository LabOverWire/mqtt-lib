use mqtt5::{AuthHandler, AuthResponse, ConnectOptions, MqttClient};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket};

const AUTH_CONTINUE: [u8; 8] = [0xF0, 6, 0x18, 4, 0x15, 0, 1, b'm'];

struct LargeResponses {
    calls: Arc<AtomicUsize>,
    size: usize,
}

impl AuthHandler for LargeResponses {
    fn handle_challenge<'a>(
        &'a self,
        _method: &'a str,
        _data: Option<&'a [u8]>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = mqtt5::Result<AuthResponse>> + Send + 'a>>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let data = vec![0u8; self.size];
        Box::pin(async move { Ok(AuthResponse::Continue(data)) })
    }
}

fn auth_server(keeps_reading: bool) -> String {
    let socket = TcpSocket::new_v4().unwrap();
    socket.set_recv_buffer_size(4096).unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = socket.local_addr().unwrap();
    let listener = socket.listen(16).unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                loop {
                    if stream.write_all(&AUTH_CONTINUE).await.is_err() {
                        return;
                    }
                    if keeps_reading {
                        let _ = tokio::time::timeout(
                            Duration::from_millis(1),
                            stream.read(&mut buffer),
                        )
                        .await;
                    } else {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                }
            });
        }
    });
    format!("mqtt://{addr}")
}

async fn connect_with_large_auth_responses(
    url: &str,
    calls: Arc<AtomicUsize>,
    wait: Duration,
) -> Option<mqtt5::Result<()>> {
    let options = ConnectOptions::new("auth-write")
        .with_automatic_reconnect(false)
        .with_authentication_method("m");
    let client = MqttClient::with_options(options.clone());
    client
        .set_auth_handler(LargeResponses {
            calls,
            size: 60_000,
        })
        .await;
    tokio::time::timeout(wait, client.connect_with_options(url, options))
        .await
        .ok()
        .map(|outcome| outcome.map(|_| ()))
}

#[tokio::test]
async fn auth_steps_end_when_the_server_reads() {
    let url = auth_server(true);
    let calls = Arc::new(AtomicUsize::new(0));
    let outcome = connect_with_large_auth_responses(&url, calls, Duration::from_secs(30)).await;
    assert!(matches!(outcome, Some(Err(_))), "{outcome:?}");
}

#[tokio::test]
async fn an_auth_response_the_server_never_reads_does_not_hold_the_client() {
    let url = auth_server(false);
    let calls = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let outcome =
        connect_with_large_auth_responses(&url, calls.clone(), Duration::from_secs(45)).await;
    let steps = calls.load(Ordering::SeqCst);
    let elapsed = started.elapsed();
    assert!(
        outcome.is_some(),
        "connect still waiting after {elapsed:?}, {steps} handler calls"
    );
}

#[tokio::test]
async fn the_auth_handler_runs_at_most_32_times() {
    let url = auth_server(true);
    let calls = Arc::new(AtomicUsize::new(0));
    let options = ConnectOptions::new("auth-count")
        .with_automatic_reconnect(false)
        .with_authentication_method("m");
    let client = MqttClient::with_options(options.clone());
    client
        .set_auth_handler(LargeResponses {
            calls: calls.clone(),
            size: 8,
        })
        .await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        client.connect_with_options(&url, options),
    )
    .await
    .expect("connect did not return");
    assert!(outcome.is_err());
    assert!(
        calls.load(Ordering::SeqCst) <= 32,
        "handler ran {} times",
        calls.load(Ordering::SeqCst)
    );
}

#[tokio::test]
async fn a_server_that_never_answers_connect_does_not_hold_the_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let client = MqttClient::new("silent-server");
    let outcome = tokio::time::timeout(
        Duration::from_secs(40),
        client.connect_with_options(
            &url,
            ConnectOptions::new("silent-server").with_automatic_reconnect(false),
        ),
    )
    .await
    .expect("connect did not return");
    assert!(
        matches!(outcome, Err(mqtt5::error::MqttError::Timeout)),
        "{outcome:?}"
    );
    assert!(!client.is_connected().await);
}

#[tokio::test]
async fn a_server_repeating_auth_packets_does_not_hold_the_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer).await;
                while stream.write_all(&[0xF0, 0x02, 0x00, 0x00]).await.is_ok() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            });
        }
    });
    let client = MqttClient::new("auth-repeated");
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        client.connect_with_options(
            &url,
            ConnectOptions::new("auth-repeated").with_automatic_reconnect(false),
        ),
    )
    .await
    .expect("connect did not return");
    assert!(outcome.is_err());
}

#[tokio::test]
async fn a_server_that_never_reads_a_large_connect_does_not_hold_the_client() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let mut large = ConnectOptions::new("large-connect").with_automatic_reconnect(false);
    for index in 0..3000 {
        large
            .properties
            .user_properties
            .push((format!("k{index}"), "v".repeat(60_000)));
    }
    let client = MqttClient::new("large-connect");
    let outcome = tokio::time::timeout(
        Duration::from_secs(40),
        client.connect_with_options(&url, large),
    )
    .await;
    assert!(outcome.is_ok(), "connect still waiting after 40s");
}
