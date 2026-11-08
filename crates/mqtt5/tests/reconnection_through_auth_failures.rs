use mqtt5::time::Duration;
use mqtt5::{AuthHandler, AuthResponse, ConnectOptions, ConnectionEvent, MqttClient};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CONNACK_OK: [u8; 5] = [0x20, 3, 0, 0, 0];
const AUTH_REAUTHENTICATE: [u8; 8] = [0xF0, 6, 0x19, 4, 0x15, 0, 1, b'X'];
const AUTH_CONTINUE: [u8; 8] = [0xF0, 6, 0x18, 4, 0x15, 0, 1, b'X'];

async fn scripted(second: &'static [u8], later: &'static [u8]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let counted = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let n = counted.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                match n {
                    0 => {
                        let _ = stream.write_all(&CONNACK_OK).await;
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                    1 => {
                        let _ = stream.write_all(second).await;
                        while stream.read(&mut buffer).await.unwrap_or(0) > 0 {}
                    }
                    _ => {
                        let _ = stream.write_all(later).await;
                        loop {
                            match stream.read(&mut buffer).await {
                                Ok(0) | Err(_) => break,
                                Ok(_) if buffer[0] == 0xF0 => {
                                    let _ = stream.write_all(&CONNACK_OK).await;
                                }
                                Ok(_) => {}
                            }
                        }
                    }
                }
            });
        }
    });
    url
}

async fn watch(client: &MqttClient) -> Arc<AtomicUsize> {
    let gave_up = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&gave_up);
    client
        .on_connection_event(move |event| {
            if let ConnectionEvent::ReconnectFailed { .. } = event {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        })
        .await
        .unwrap();
    gave_up
}

#[tokio::test]
async fn a_stray_auth_from_the_server_during_a_reconnection_keeps_reconnecting() {
    let url = scripted(&AUTH_REAUTHENTICATE, &CONNACK_OK).await;
    let options = ConnectOptions::new("stray-auth")
        .with_reconnect_delay(Duration::from_millis(200), Duration::from_millis(200));
    let client = MqttClient::with_options(options.clone());
    let gave_up = watch(&client).await;
    client.connect_with_options(&url, options).await.unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(client.is_connected().await);
    assert_eq!(gave_up.load(Ordering::SeqCst), 0);
}

struct FlakyHandler {
    calls: AtomicUsize,
}

impl AuthHandler for FlakyHandler {
    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        _challenge_data: Option<&'a [u8]>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = mqtt5::Result<AuthResponse>> + Send + 'a>>
    {
        Box::pin(async move {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(AuthResponse::Abort("token service unreachable".to_string()))
            } else {
                Ok(AuthResponse::Continue(b"token".to_vec()))
            }
        })
    }
}

#[tokio::test]
async fn an_auth_handler_abort_during_a_reconnection_keeps_reconnecting() {
    let url = scripted(&AUTH_CONTINUE, &AUTH_CONTINUE).await;
    let options = ConnectOptions::new("flaky-handler")
        .with_authentication_method("X")
        .with_reconnect_delay(Duration::from_millis(200), Duration::from_millis(200));
    let client = MqttClient::with_options(options.clone());
    client
        .set_auth_handler(FlakyHandler {
            calls: AtomicUsize::new(0),
        })
        .await;
    let gave_up = watch(&client).await;
    client.connect_with_options(&url, options).await.unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(client.is_connected().await);
    assert_eq!(gave_up.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_stray_auth_answering_a_connect_that_replaced_a_reconnection_keeps_reconnecting() {
    let url = scripted(&AUTH_REAUTHENTICATE, &CONNACK_OK).await;
    let options = ConnectOptions::new("stray-auth-connect")
        .with_reconnect_delay(Duration::from_millis(1500), Duration::from_millis(1500));
    let client = MqttClient::with_options(options.clone());
    let gave_up = watch(&client).await;
    client
        .connect_with_options(&url, options.clone())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(!client.is_connected().await);
    let _ = client.connect_with_options(&url, options).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(client.is_connected().await);
    assert_eq!(gave_up.load(Ordering::SeqCst), 0);
}

struct OversizedOnce {
    calls: AtomicUsize,
}

impl AuthHandler for OversizedOnce {
    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        _challenge_data: Option<&'a [u8]>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = mqtt5::Result<AuthResponse>> + Send + 'a>>
    {
        Box::pin(async move { Ok(AuthResponse::Success) })
    }

    fn initial_response<'a>(
        &'a self,
        _auth_method: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = mqtt5::Result<Option<Vec<u8>>>> + Send + 'a>,
    > {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(Some(if call == 1 {
                vec![b'x'; 70_000]
            } else {
                b"token".to_vec()
            }))
        })
    }
}

#[tokio::test]
async fn an_initial_response_too_large_for_one_reconnection_keeps_reconnecting() {
    let url = scripted(&CONNACK_OK, &CONNACK_OK).await;
    let options = ConnectOptions::new("oversized-once")
        .with_authentication_method("token")
        .with_reconnect_delay(Duration::from_millis(200), Duration::from_millis(200));
    let client = MqttClient::with_options(options.clone());
    client
        .set_auth_handler(OversizedOnce {
            calls: AtomicUsize::new(0),
        })
        .await;
    let gave_up = watch(&client).await;
    client.connect_with_options(&url, options).await.unwrap();
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(client.is_connected().await);
    assert_eq!(gave_up.load(Ordering::SeqCst), 0);
}
