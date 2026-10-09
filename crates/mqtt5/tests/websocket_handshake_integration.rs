#![cfg(feature = "transport-websocket")]

//! Checks the WebSocket upgrade request the client transport actually sends,
//! as seen by a server, rather than the configuration it was built from.

use mqtt5::transport::websocket::{WebSocketConfig, WebSocketTransport};
use mqtt5::Transport;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::http::HeaderMap;

const WAIT: Duration = Duration::from_secs(5);

/// Records the upgrade request's headers and selects the first offered
/// subprotocol, as a broker would.
struct CaptureRequest {
    captured: Arc<Mutex<Option<HeaderMap>>>,
}

impl Callback for CaptureRequest {
    fn on_request(self, req: &Request, mut resp: Response) -> Result<Response, ErrorResponse> {
        if let Some(first) = req
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
        {
            resp.headers_mut().insert(
                "Sec-WebSocket-Protocol",
                first.trim().parse().expect("header"),
            );
        }
        *self.captured.lock().expect("lock") = Some(req.headers().clone());
        Ok(resp)
    }
}

/// Connects a transport built by `configure` to a loopback server and returns
/// the headers of the upgrade request the server received, with the address
/// the server listened on.
async fn received_headers(
    configure: impl FnOnce(WebSocketConfig) -> WebSocketConfig,
) -> (HeaderMap, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let captured = Arc::new(Mutex::new(None));
    let callback = CaptureRequest {
        captured: Arc::clone(&captured),
    };
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        tokio_tungstenite::accept_hdr_async(stream, callback)
            .await
            .expect("server handshake")
    });

    let config = configure(WebSocketConfig::new(&format!("ws://{addr}/mqtt")).expect("config"));
    let mut transport = WebSocketTransport::new(config);
    timeout(WAIT, transport.connect())
        .await
        .expect("connect timed out")
        .expect("client handshake");
    let _server_ws = timeout(WAIT, server)
        .await
        .expect("server timed out")
        .expect("server task");
    transport.close().await.expect("close");

    let headers = captured.lock().expect("lock").take();
    (headers.expect("server saw no upgrade request"), addr)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|v| v.to_str().expect("ascii header"))
}

#[tokio::test]
async fn upgrade_request_carries_configured_headers_subprotocols_and_user_agent() {
    let (headers, _) = received_headers(|config| {
        config
            .with_header("Authorization", "Bearer token123")
            .with_header("X-Custom", "expected")
            .with_subprotocols(&["mqttv5.0", "mqtt"])
            .with_user_agent("repro/1.0")
    })
    .await;

    assert_eq!(header(&headers, "Authorization"), Some("Bearer token123"));
    assert_eq!(header(&headers, "X-Custom"), Some("expected"));
    assert_eq!(
        header(&headers, "Sec-WebSocket-Protocol"),
        Some("mqttv5.0, mqtt")
    );
    assert_eq!(header(&headers, "User-Agent"), Some("repro/1.0"));
}

#[tokio::test]
async fn upgrade_request_defaults_offer_mqtt_and_crate_user_agent() {
    let (headers, addr) = received_headers(|config| config).await;

    assert_eq!(header(&headers, "Sec-WebSocket-Protocol"), Some("mqtt"));
    assert_eq!(
        header(&headers, "User-Agent"),
        Some(concat!("mqtt5/", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(header(&headers, "Host"), Some(addr.to_string().as_str()));
}

#[tokio::test]
async fn invalid_header_fails_connect_before_dialing() {
    let config = WebSocketConfig::new("ws://127.0.0.1:9/mqtt")
        .expect("config")
        .with_header("Host", "spoofed.example.com");
    let mut transport = WebSocketTransport::new(config);

    let err = transport
        .connect()
        .await
        .expect_err("reserved header accepted");
    assert!(
        err.to_string().contains("reserved for the handshake"),
        "unexpected error: {err}"
    );
    assert!(!transport.is_connected());
}

/// The broker in this crate only selects a subprotocol the client offers, and
/// the client fails the handshake when the server selects none, so a client
/// that preferred `mqttv5.0` could only connect because `mqtt` is still
/// offered (MQTT-6.0.0-3).
#[cfg(feature = "broker")]
#[tokio::test]
async fn preferring_another_subprotocol_still_connects_to_the_broker() {
    use mqtt5::broker::config::{
        BrokerConfig, StorageBackend, StorageConfig, WebSocketConfig as BrokerWebSocketConfig,
    };
    use mqtt5::broker::MqttBroker;

    let config = BrokerConfig::default()
        .with_storage(StorageConfig::new().with_backend(StorageBackend::Memory))
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_websocket(
            BrokerWebSocketConfig::new()
                .with_bind_address(([127, 0, 0, 1], 0))
                .with_path("/mqtt"),
        );
    let mut broker = MqttBroker::with_config(config).await.expect("broker");
    let addr = broker.ws_local_addr().expect("websocket listener");
    let mut ready = broker.ready_receiver();
    let broker_task = tokio::spawn(async move { broker.run().await });
    timeout(WAIT, ready.wait_for(|ready| *ready))
        .await
        .expect("broker did not become ready")
        .expect("broker stopped");

    let config = WebSocketConfig::new(&format!("ws://{addr}/mqtt"))
        .expect("config")
        .with_subprotocol("mqttv5.0");
    let mut transport = WebSocketTransport::new(config);
    timeout(WAIT, transport.connect())
        .await
        .expect("connect timed out")
        .expect("broker refused a client preferring mqttv5.0");

    transport.close().await.expect("close");
    broker_task.abort();
}
