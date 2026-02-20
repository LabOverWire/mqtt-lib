use futures_util::{SinkExt, StreamExt};
use mqtt5_conformance::harness::ConformanceBroker;
use mqtt5_conformance::raw_client::RawPacketBuilder;
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const TIMEOUT: Duration = Duration::from_secs(3);

async fn ws_connect_raw(
    broker: &ConformanceBroker,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let ws_port = broker.ws_port().expect("WebSocket not enabled");
    let url = format!("ws://127.0.0.1:{ws_port}/mqtt");
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
    broker: &ConformanceBroker,
) -> (
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    http::Response<Option<Vec<u8>>>,
) {
    let ws_port = broker.ws_port().expect("WebSocket not enabled");
    let url = format!("ws://127.0.0.1:{ws_port}/mqtt");
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
#[tokio::test]
async fn websocket_text_frame_closes() {
    let broker = ConformanceBroker::start_with_websocket().await;
    let mut ws = ws_connect_raw(&broker).await;

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
#[tokio::test]
async fn websocket_packet_across_frames() {
    let broker = ConformanceBroker::start_with_websocket().await;
    let mut ws = ws_connect_raw(&broker).await;

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
#[tokio::test]
async fn websocket_subprotocol_is_mqtt() {
    let broker = ConformanceBroker::start_with_websocket().await;
    let (_ws, response) = ws_connect_raw_with_response(&broker).await;

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
