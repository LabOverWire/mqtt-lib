#![cfg(feature = "transport-websocket")]

//! Checks that `ConnectOptions::with_websocket_config` reaches the upgrade
//! request of every connection `MqttClient` makes, including automatic
//! reconnects, as seen by a server.
//!
//! The wss:// test requires the certificates from
//! `scripts/generate_test_certs.sh`.

mod wss_test_server;

use futures::{SinkExt, StreamExt};
use mqtt5::time::Duration;
use mqtt5::transport::websocket::WebSocketConfig;
use mqtt5::{ConnectOptions, MqttClient};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::http::HeaderMap;
use tokio_tungstenite::tungstenite::Message;
use wss_test_server::{certs, key, server_config, wss_server, CA, SERVER_CERT, SERVER_KEY, WAIT};

/// An MQTT v5 CONNACK accepting the connection: no session, success, no
/// properties.
const CONNACK: [u8; 5] = [0x20, 0x03, 0x00, 0x00, 0x00];

/// Sends each upgrade request's headers to the test and selects the first
/// offered subprotocol, as a broker would.
struct Capture(mpsc::UnboundedSender<HeaderMap>);

impl Callback for Capture {
    fn on_request(self, req: &Request, mut resp: Response) -> Result<Response, ErrorResponse> {
        let _ = self.0.send(req.headers().clone());
        if let Some(first) = req
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
        {
            let first = first.trim().parse().expect("subprotocol header value");
            resp.headers_mut().insert("Sec-WebSocket-Protocol", first);
        }
        Ok(resp)
    }
}

/// Starts a ws:// server that answers each connection's CONNECT with
/// `connack` and then closes it, so the client reconnects. The receiver yields
/// the headers of every upgrade request.
async fn closing_mqtt_server(connack: Vec<u8>) -> (String, mpsc::UnboundedReceiver<HeaderMap>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let capture = Capture(tx.clone());
            let connack = connack.clone();
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, capture).await else {
                    return;
                };
                if !matches!(ws.next().await, Some(Ok(Message::Binary(_)))) {
                    return;
                }
                let _ = ws.send(Message::Binary(connack.into())).await;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            });
        }
    });
    (format!("ws://{addr}/mqtt"), rx)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).map(|v| v.to_str().expect("ascii header"))
}

#[tokio::test]
async fn websocket_config_is_sent_on_connect_and_on_reconnect() {
    let (url, mut requests) = closing_mqtt_server(CONNACK.to_vec()).await;
    let websocket = WebSocketConfig::new("ws://placeholder.invalid/other")
        .expect("config")
        .with_header("Authorization", "Bearer token123")
        .with_subprotocol("mqttv5.0")
        .with_user_agent("configured/1.0");
    let options = ConnectOptions::new("ws-config-client")
        .with_websocket_config(websocket)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(500));
    let client = MqttClient::with_options(options);

    timeout(WAIT, client.connect(&url))
        .await
        .expect("connect timed out")
        .expect("connect");

    for attempt in ["connect", "reconnect"] {
        let headers = timeout(WAIT, requests.recv())
            .await
            .unwrap_or_else(|_| panic!("no upgrade request for the {attempt}"))
            .expect("server stopped");
        assert_eq!(
            header(&headers, "Authorization"),
            Some("Bearer token123"),
            "{attempt}"
        );
        assert_eq!(
            header(&headers, "Sec-WebSocket-Protocol"),
            Some("mqttv5.0, mqtt"),
            "{attempt}"
        );
        assert_eq!(
            header(&headers, "User-Agent"),
            Some("configured/1.0"),
            "{attempt}"
        );
        assert_eq!(
            header(&headers, "Host"),
            url.strip_prefix("ws://")
                .and_then(|r| r.strip_suffix("/mqtt")),
            "{attempt}"
        );
    }

    let _ = client.disconnect().await;
}

fn redirect_connack(server_reference: &str) -> Vec<u8> {
    let reference = server_reference.as_bytes();
    let reference_len = u16::try_from(reference.len()).expect("reference length");
    let properties_len = u8::try_from(3 + reference.len()).expect("properties length");
    let mut connack = vec![0x20, properties_len + 3, 0x00, 0x9C, properties_len, 0x1C];
    connack.extend_from_slice(&reference_len.to_be_bytes());
    connack.extend_from_slice(reference);
    connack
}

#[tokio::test]
async fn websocket_config_is_not_sent_to_a_redirect_target() {
    let (target_url, mut target_requests) = closing_mqtt_server(CONNACK.to_vec()).await;
    let (origin_url, mut origin_requests) =
        closing_mqtt_server(redirect_connack(&target_url)).await;
    let websocket = WebSocketConfig::new("ws://placeholder.invalid/other")
        .expect("config")
        .with_header("Authorization", "Bearer token123");
    let client = MqttClient::with_options(
        ConnectOptions::new("ws-redirect-client")
            .with_websocket_config(websocket)
            .with_automatic_reconnect(false),
    );

    timeout(WAIT, client.connect(&origin_url))
        .await
        .expect("connect timed out")
        .expect("connect");

    let origin = timeout(WAIT, origin_requests.recv())
        .await
        .expect("no upgrade request for the origin")
        .expect("origin server stopped");
    assert_eq!(header(&origin, "Authorization"), Some("Bearer token123"));
    let target = timeout(WAIT, target_requests.recv())
        .await
        .expect("no upgrade request for the redirect target")
        .expect("target server stopped");
    assert_eq!(header(&target, "Authorization"), None);

    let _ = client.disconnect().await;
}

#[tokio::test]
async fn websocket_config_tls_is_used_for_wss() {
    let (addr, server) = wss_server(server_config(
        certs(SERVER_CERT),
        key(SERVER_KEY),
        false,
        &[],
    ))
    .await;
    let websocket = WebSocketConfig::new("wss://localhost/mqtt")
        .expect("config")
        .with_ca_cert_from_file(CA)
        .expect("load CA");
    let client = MqttClient::with_options(
        ConnectOptions::new("wss-config-client")
            .with_websocket_config(websocket)
            .with_automatic_reconnect(false),
    );

    let connecting = tokio::spawn(async move {
        let _ = client.connect(&format!("wss://{addr}/mqtt")).await;
    });
    timeout(WAIT, server)
        .await
        .expect("server timed out")
        .expect("server saw no completed handshake: the configured CA was not used");
    connecting.abort();
}
