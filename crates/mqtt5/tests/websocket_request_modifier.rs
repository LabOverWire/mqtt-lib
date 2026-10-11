#![cfg(feature = "transport-websocket")]

//! Checks that `WebSocketConfig::with_request_modifier` adjusts the upgrade
//! request the transport sends, as seen by a server.

use mqtt5::error::MqttError;
use mqtt5::transport::websocket::{WebSocketConfig, WebSocketTransport};
use mqtt5::{ConnectOptions, MqttClient, Transport};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::http::{HeaderMap, HeaderValue, Uri};

const WAIT: Duration = Duration::from_secs(5);

/// What the server saw of the upgrade request.
type Seen = Arc<Mutex<Option<(Uri, HeaderMap)>>>;

/// Records the upgrade request and selects "mqtt", as a broker would.
struct Capture(Seen);

impl Callback for Capture {
    fn on_request(self, req: &Request, mut resp: Response) -> Result<Response, ErrorResponse> {
        *self.0.lock().expect("lock") = Some((req.uri().clone(), req.headers().clone()));
        resp.headers_mut()
            .insert("Sec-WebSocket-Protocol", HeaderValue::from_static("mqtt"));
        Ok(resp)
    }
}

/// Connects a transport with `config` to a one-connection loopback server
/// and returns the request the server received.
async fn received(config: impl FnOnce(String) -> WebSocketConfig) -> (Uri, HeaderMap) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen = Seen::default();
    let capture = Capture(Arc::clone(&seen));
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        tokio_tungstenite::accept_hdr_async(stream, capture)
            .await
            .expect("server handshake")
    });

    let mut transport = WebSocketTransport::new(config(format!("ws://{addr}/mqtt")));
    timeout(WAIT, transport.connect())
        .await
        .expect("connect timed out")
        .expect("client handshake");
    let _server_ws = timeout(WAIT, server)
        .await
        .expect("server timed out")
        .expect("server task");
    transport.close().await.expect("close");

    let request = seen.lock().expect("lock").take();
    request.expect("server saw no upgrade request")
}

#[tokio::test]
async fn modifier_adds_headers_computed_at_connect_time() {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let (_, headers) = received(|url| {
        WebSocketConfig::new(&url)
            .expect("config")
            .with_header("X-Static", "configured")
            .with_request_modifier(move |mut request| {
                let call = counter.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    request
                        .headers_mut()
                        .insert("x-token", HeaderValue::from(call));
                    Ok::<_, std::convert::Infallible>(request)
                }
            })
    })
    .await;

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        headers.get("x-token").and_then(|v| v.to_str().ok()),
        Some("1")
    );
    assert_eq!(
        headers.get("x-static").and_then(|v| v.to_str().ok()),
        Some("configured")
    );
}

#[tokio::test]
async fn modifier_can_replace_the_uri() {
    let (uri, _) = received(|url| {
        WebSocketConfig::new(&url)
            .expect("config")
            .with_request_modifier(|mut request| async move {
                let presigned = format!("{}?X-Amz-Signature=abc123", request.uri());
                *request.uri_mut() = presigned.parse()?;
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(request)
            })
    })
    .await;

    assert_eq!(uri.path(), "/mqtt");
    assert_eq!(uri.query(), Some("X-Amz-Signature=abc123"));
}

#[tokio::test]
async fn modifier_error_fails_connect_before_dialing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let config = WebSocketConfig::new(&format!("ws://{addr}/mqtt"))
        .expect("config")
        .with_request_modifier(|_request| async {
            Err::<Request, _>(std::io::Error::other("token service unavailable"))
        });
    let mut transport = WebSocketTransport::new(config);

    let err = timeout(WAIT, transport.connect())
        .await
        .expect("connect timed out")
        .expect_err("connect succeeded despite the modifier failing");
    assert!(
        matches!(&err, MqttError::ConnectionError(msg)
            if msg.contains("request modifier failed") && msg.contains("token service unavailable")),
        "unexpected error: {err}"
    );
    assert!(!transport.is_connected());
    assert!(
        timeout(Duration::from_millis(200), listener.accept())
            .await
            .is_err(),
        "the transport dialed although the modifier failed"
    );
}

#[tokio::test]
async fn modifier_runs_within_the_connect_timeout() {
    let config = WebSocketConfig::new("ws://127.0.0.1:9/mqtt")
        .expect("config")
        .with_timeout(mqtt5::time::Duration::from_millis(200))
        .with_request_modifier(|_request| {
            std::future::pending::<Result<Request, std::io::Error>>()
        });
    let mut transport = WebSocketTransport::new(config);

    let err = timeout(WAIT, transport.connect())
        .await
        .expect("the modifier was not bounded by the connect timeout")
        .expect_err("connect succeeded with a modifier that never finishes");
    assert!(matches!(err, MqttError::Timeout), "unexpected error: {err}");
}

async fn rejected_before_dialing(modify: fn(&mut Request)) -> MqttError {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let config = WebSocketConfig::new(&format!("ws://{addr}/mqtt"))
        .expect("config")
        .with_request_modifier(move |mut request| {
            modify(&mut request);
            async move { Ok::<_, std::convert::Infallible>(request) }
        });
    let mut transport = WebSocketTransport::new(config);
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        tokio_tungstenite::accept_hdr_async(stream, Capture(Seen::default())).await
    });

    let err = timeout(WAIT, transport.connect())
        .await
        .expect("connect timed out")
        .expect_err("connect sent a request the modifier made invalid");
    assert!(
        timeout(Duration::from_millis(200), server).await.is_err(),
        "the transport dialed although the modified request is invalid"
    );
    err
}

fn assert_configuration_error(err: &MqttError, expected: &str) {
    assert!(
        matches!(err, MqttError::Configuration(msg) if msg.contains(expected)),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn modifier_cannot_add_a_reserved_header() {
    let err = rejected_before_dialing(|request| {
        request
            .headers_mut()
            .insert("content-length", HeaderValue::from_static("5"));
    })
    .await;
    assert_configuration_error(&err, "content-length");
}

#[tokio::test]
async fn modifier_cannot_repeat_a_handshake_header() {
    let err = rejected_before_dialing(|request| {
        request
            .headers_mut()
            .append("host", HeaderValue::from_static("elsewhere.example"));
    })
    .await;
    assert_configuration_error(&err, "host");
}

#[tokio::test]
async fn modifier_cannot_stop_offering_mqtt() {
    let err = rejected_before_dialing(|request| {
        request
            .headers_mut()
            .insert("sec-websocket-protocol", HeaderValue::from_static("mqttv5"));
    })
    .await;
    assert_configuration_error(&err, "mqtt");
}

#[tokio::test]
async fn modifier_cannot_remove_the_subprotocol_header() {
    let err = rejected_before_dialing(|request| {
        request.headers_mut().remove("sec-websocket-protocol");
    })
    .await;
    assert_configuration_error(&err, "Sec-WebSocket-Protocol");
}

#[tokio::test]
async fn modifier_cannot_change_the_scheme() {
    let err = rejected_before_dialing(|request| {
        let secure = request.uri().to_string().replacen("ws://", "wss://", 1);
        *request.uri_mut() = secure.parse().expect("uri");
    })
    .await;
    assert_configuration_error(&err, "scheme");
}

fn client_with(config: WebSocketConfig) -> MqttClient {
    MqttClient::with_options(
        ConnectOptions::new("ws-modifier-errors")
            .with_websocket_config(config)
            .with_automatic_reconnect(false),
    )
}

#[tokio::test]
async fn client_reports_a_hung_modifier_as_a_timeout() {
    let config = WebSocketConfig::new("ws://127.0.0.1:9/mqtt")
        .expect("config")
        .with_timeout(mqtt5::time::Duration::from_millis(200))
        .with_request_modifier(|_request| {
            std::future::pending::<Result<Request, std::io::Error>>()
        });
    let client = client_with(config);

    let err = timeout(WAIT, client.connect("ws://127.0.0.1:9/mqtt"))
        .await
        .expect("connect finishes")
        .expect_err("connect succeeded with a modifier that never finishes");
    assert!(matches!(err, MqttError::Timeout), "unexpected error: {err}");
}

#[tokio::test]
async fn client_reports_a_modifier_error_once() {
    let config = WebSocketConfig::new("ws://127.0.0.1:9/mqtt")
        .expect("config")
        .with_request_modifier(|_request| async {
            Err::<Request, _>(std::io::Error::other("token service unavailable"))
        });
    let client = client_with(config);

    let err = timeout(WAIT, client.connect("ws://127.0.0.1:9/mqtt"))
        .await
        .expect("connect finishes")
        .expect_err("connect succeeded despite the modifier failing");
    assert!(
        matches!(&err, MqttError::ConnectionError(msg)
            if msg == "WebSocket request modifier failed: token service unavailable"),
        "unexpected error: {err:?}"
    );
}
