use mqtt5::error::MqttError;
use mqtt5::{AuthHandler, AuthResponse, ConnectOptions, ConnectionEvent, MqttClient};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
async fn the_auth_handler_runs_32_times_then_the_connect_fails() {
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
    assert_eq!(calls.load(Ordering::SeqCst), 32);
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

const CONNACK_OK: [u8; 5] = [0x20, 3, 0x00, 0x00, 0x00];

async fn read_some(stream: &mut tokio::net::TcpStream) {
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf).await;
}

async fn connack_after(delay: Duration) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((mut stream, _)) = listener.accept().await {
            read_some(&mut stream).await;
            tokio::time::sleep(delay).await;
            stream.write_all(&CONNACK_OK).await.unwrap();
            held.push(stream);
        }
    });
    url
}

#[tokio::test]
async fn thirty_two_auth_steps_then_connack_succeed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_some(&mut stream).await;
        for _ in 0..32 {
            stream.write_all(&AUTH_CONTINUE).await.unwrap();
            read_some(&mut stream).await;
        }
        stream.write_all(&CONNACK_OK).await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let calls = Arc::new(AtomicUsize::new(0));
    let options = ConnectOptions::new("auth-boundary")
        .with_automatic_reconnect(false)
        .with_authentication_method("m");
    let client = MqttClient::with_options(options.clone());
    client
        .set_auth_handler(LargeResponses {
            calls: calls.clone(),
            size: 8,
        })
        .await;
    client.connect_with_options(&url, options).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 32);
}

#[tokio::test]
async fn the_limit_can_be_lowered_and_raised() {
    let url = connack_after(Duration::from_millis(300)).await;
    let short = ConnectOptions::new("limit-short")
        .with_automatic_reconnect(false)
        .with_connect_packet_timeout(Duration::from_millis(50));
    let started = Instant::now();
    let outcome = MqttClient::with_options(short.clone())
        .connect_with_options(&url, short)
        .await;
    assert!(matches!(outcome, Err(MqttError::Timeout)), "{outcome:?}");
    assert!(started.elapsed() < Duration::from_millis(300));

    let long = ConnectOptions::new("limit-long")
        .with_automatic_reconnect(false)
        .with_connect_packet_timeout(Duration::from_secs(5));
    MqttClient::with_options(long.clone())
        .connect_with_options(&url, long)
        .await
        .unwrap();
}

#[tokio::test]
async fn options_built_from_default_keep_the_limit_of_30_seconds() {
    let url = connack_after(Duration::from_millis(100)).await;
    let options = ConnectOptions {
        protocol_options: ConnectOptions::new("limit-default").protocol_options,
        ..ConnectOptions::default()
    }
    .with_automatic_reconnect(false);
    assert_eq!(options.connect_packet_timeout, None);
    MqttClient::with_options(options.clone())
        .connect_with_options(&url, options)
        .await
        .unwrap();
}

struct ReconnectRecord {
    failed: Arc<AtomicBool>,
    attempts: Arc<Mutex<Vec<u32>>>,
}

async fn record_reconnects(client: &MqttClient) -> ReconnectRecord {
    let failed = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let (seen_failed, seen_attempts) = (failed.clone(), attempts.clone());
    client
        .on_connection_event(move |event| match event {
            ConnectionEvent::ReconnectFailed { .. } => seen_failed.store(true, Ordering::SeqCst),
            ConnectionEvent::Reconnecting { attempt } => {
                seen_attempts.lock().unwrap().push(attempt);
            }
            _ => {}
        })
        .await
        .unwrap();
    ReconnectRecord { failed, attempts }
}

fn reconnecting(client_id: &str, attempts: u32) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(10), Duration::from_millis(10))
        .with_max_reconnect_attempts(attempts)
        .with_connect_packet_timeout(Duration::from_millis(100))
}

#[tokio::test]
async fn max_reconnect_attempts_ends_reconnection_to_a_silent_server() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let counted = accepted.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            held.push(stream);
        }
    });
    let options = reconnecting("attempts-silent", 2);
    let client = MqttClient::with_options(options.clone());
    let record = record_reconnects(&client).await;
    let outcome = client.connect_with_options(&url, options).await;
    assert!(matches!(outcome, Err(MqttError::Timeout)), "{outcome:?}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(
        record.failed.load(Ordering::SeqCst),
        "accepted {} connections, attempts {:?}",
        accepted.load(Ordering::SeqCst),
        record.attempts.lock().unwrap()
    );
    assert_eq!(*record.attempts.lock().unwrap(), vec![1, 2]);
    assert_eq!(accepted.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_successful_connect_restarts_the_attempts() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("mqtt://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            read_some(&mut stream).await;
            stream.write_all(&CONNACK_OK).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let options = reconnecting("attempts-restart", 2);
    let client = MqttClient::with_options(options.clone());
    let record = record_reconnects(&client).await;
    client.connect_with_options(&url, options).await.unwrap();
    let started = Instant::now();
    while record.attempts.lock().unwrap().len() < 3 && started.elapsed() < Duration::from_secs(15) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let attempts = record.attempts.lock().unwrap().clone();
    assert!(attempts.len() >= 3, "{attempts:?}");
    assert!(attempts.iter().all(|attempt| *attempt == 1), "{attempts:?}");
    assert!(!record.failed.load(Ordering::SeqCst));
}

#[cfg(feature = "transport-quic")]
mod quic {
    use super::*;
    use quinn::crypto::rustls::QuicServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    const CONNACK_SESSION_PRESENT: [u8; 5] = [0x20, 3, 0x01, 0x00, 0x00];

    fn endpoint(stream_window: u32) -> quinn::Endpoint {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let chain = vec![CertificateDer::from(cert.cert.der().to_vec())];
        let key = PrivateKeyDer::try_from(cert.signing_key.serialize_der()).unwrap();
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        tls.alpn_protocols = vec![b"mqtt".to_vec()];
        let mut server =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls).unwrap()));
        let mut transport = quinn::TransportConfig::default();
        transport.stream_receive_window(stream_window.into());
        transport.receive_window(1_048_576u32.into());
        transport.keep_alive_interval(Some(Duration::from_secs(5)));
        server.transport_config(Arc::new(transport));
        quinn::Endpoint::server(server, "127.0.0.1:0".parse().unwrap()).unwrap()
    }

    async fn connect_size(options: ConnectOptions) -> usize {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("mqtt://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let size = stream.read(&mut buf).await.unwrap();
            stream.write_all(&CONNACK_OK).await.unwrap();
            tokio::time::sleep(Duration::from_millis(200)).await;
            size
        });
        let client = MqttClient::with_options(options.clone());
        client.connect_with_options(&url, options).await.unwrap();
        client.disconnect().await.unwrap();
        server.await.unwrap()
    }

    async fn session_present_outcome(
        stream_window: u32,
        options: ConnectOptions,
    ) -> mqtt5::Result<()> {
        let server = endpoint(stream_window);
        let url = format!("quic://{}", server.local_addr().unwrap());
        tokio::spawn(async move {
            let connection = server.accept().await.unwrap().await.unwrap();
            let (mut send, recv) = connection.accept_bi().await.unwrap();
            send.write_all(&CONNACK_SESSION_PRESENT).await.unwrap();
            let _ = connection.closed().await;
            drop(recv);
        });
        let client = MqttClient::with_options(options.clone());
        let outcome = tokio::time::timeout(
            Duration::from_secs(10),
            client.connect_with_options(&url, options),
        )
        .await
        .expect("connect still waiting");
        assert!(
            tokio::time::timeout(Duration::from_secs(1), client.is_connected())
                .await
                .is_ok(),
            "is_connected still waiting"
        );
        outcome.map(|_| ())
    }

    fn options() -> ConnectOptions {
        ConnectOptions::new("sp")
            .with_automatic_reconnect(false)
            .with_connect_packet_timeout(Duration::from_secs(1))
    }

    #[tokio::test]
    async fn control_an_unexpected_session_present_is_refused() {
        let outcome = session_present_outcome(1_000_000, options()).await;
        assert!(
            matches!(outcome, Err(MqttError::ProtocolError(_))),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn the_disconnect_for_an_unexpected_session_present_has_the_limit() {
        let size = connect_size(options()).await;
        let outcome = session_present_outcome(u32::try_from(size).unwrap(), options()).await;
        assert!(
            matches!(outcome, Err(MqttError::ProtocolError(_))),
            "{outcome:?}"
        );
    }
}
