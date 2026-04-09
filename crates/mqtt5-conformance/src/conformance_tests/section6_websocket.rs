//! Section 6 — MQTT over WebSocket.

use crate::conformance_test;
use crate::raw_client::RawPacketBuilder;
use crate::sut::SutHandle;
use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const TIMEOUT: Duration = Duration::from_secs(3);

fn ws_addr(sut: &SutHandle) -> SocketAddr {
    sut.expect_ws_addr()
}

async fn ws_connect_raw(
    sut: &SutHandle,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let addr = ws_addr(sut);
    let url = format!("ws://{addr}/mqtt");
    let mut request = url.into_client_request().expect("valid request");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        http::HeaderValue::from_static("mqtt"),
    );
    let (ws_stream, _response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("WebSocket connect failed");
    ws_stream
}

async fn ws_connect_raw_with_response(
    sut: &SutHandle,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    http::Response<Option<Vec<u8>>>,
) {
    let addr = ws_addr(sut);
    let url = format!("ws://{addr}/mqtt");
    let mut request = url.into_client_request().expect("valid request");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        http::HeaderValue::from_static("mqtt"),
    );
    tokio_tungstenite::connect_async(request)
        .await
        .expect("WebSocket connect failed")
}

/// `[MQTT-6.0.0-1]` Server MUST close the connection if a non-binary
/// data frame is received.
#[conformance_test(
    ids = ["MQTT-6.0.0-1"],
    requires = ["transport.websocket"],
)]
async fn websocket_text_frame_closes(sut: SutHandle) {
    let mut ws = ws_connect_raw(&sut).await;

    let connect_bytes = RawPacketBuilder::valid_connect("ws-text-test");
    ws.send(Message::Binary(connect_bytes.into()))
        .await
        .expect("send CONNECT");

    let connack = tokio::time::timeout(TIMEOUT, ws.next()).await;
    assert!(
        connack.is_ok(),
        "should receive CONNACK before text frame test"
    );

    ws.send(Message::Text("not a binary frame".into()))
        .await
        .expect("send text frame");

    let result = tokio::time::timeout(TIMEOUT, ws.next()).await;
    match result {
        Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) | Err(_) => {}
        Ok(Some(Ok(msg))) => {
            panic!("[MQTT-6.0.0-1] server must close connection on text frame, got: {msg:?}");
        }
    }
}

/// `[MQTT-6.0.0-2]` Server MUST NOT assume MQTT packets are aligned on
/// WebSocket frame boundaries; partial packets across frames must work.
#[conformance_test(
    ids = ["MQTT-6.0.0-2"],
    requires = ["transport.websocket"],
)]
async fn websocket_packet_across_frames(sut: SutHandle) {
    let mut ws = ws_connect_raw(&sut).await;

    let connect_bytes = RawPacketBuilder::valid_connect("ws-split-test");
    let mid = connect_bytes.len() / 2;
    let first_half = &connect_bytes[..mid];
    let second_half = &connect_bytes[mid..];

    ws.send(Message::Binary(first_half.to_vec().into()))
        .await
        .expect("send first half");
    ws.send(Message::Binary(second_half.to_vec().into()))
        .await
        .expect("send second half");

    let response = tokio::time::timeout(TIMEOUT, ws.next()).await;
    match response {
        Ok(Some(Ok(Message::Binary(data)))) => {
            assert!(
                data.len() >= 4,
                "[MQTT-6.0.0-2] expected CONNACK packet, got too-short response"
            );
            assert_eq!(
                data[0], 0x20,
                "[MQTT-6.0.0-2] expected CONNACK (0x20), got {:#04x}",
                data[0]
            );
        }
        other => {
            panic!(
                "[MQTT-6.0.0-2] server should reassemble split CONNECT and reply with CONNACK, got: {other:?}"
            );
        }
    }
}

/// `[MQTT-6.0.0-4]` Server MUST select "mqtt" as the WebSocket subprotocol.
#[conformance_test(
    ids = ["MQTT-6.0.0-4"],
    requires = ["transport.websocket"],
)]
async fn websocket_subprotocol_is_mqtt(sut: SutHandle) {
    let (_ws, response) = ws_connect_raw_with_response(&sut).await;

    let subprotocol = response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .expect("[MQTT-6.0.0-4] server must include Sec-WebSocket-Protocol header");

    assert_eq!(
        subprotocol.to_str().unwrap(),
        "mqtt",
        "[MQTT-6.0.0-4] server must select 'mqtt' subprotocol"
    );
}
