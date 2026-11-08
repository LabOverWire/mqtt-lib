#![cfg(feature = "transport-websocket")]

use futures_util::{SinkExt, StreamExt};
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::Message;

const HANDSHAKE_DELAY: Duration = Duration::from_secs(1);

struct OfferingMqtt;

impl Callback for OfferingMqtt {
    fn on_request(self, _: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());
        Ok(response)
    }
}

async fn unused_address() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
}

async fn slow_accepting_server(addr: SocketAddr) -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind(addr).await.unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let connects = Arc::new(AtomicUsize::new(0));
    let (accepting, connecting) = (Arc::clone(&accepted), Arc::clone(&connects));
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            accepting.fetch_add(1, Ordering::SeqCst);
            let connecting = Arc::clone(&connecting);
            tokio::spawn(async move {
                tokio::time::sleep(HANDSHAKE_DELAY).await;
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(stream, OfferingMqtt).await
                else {
                    return;
                };
                if let Some(Ok(_)) = ws.next().await {
                    connecting.fetch_add(1, Ordering::SeqCst);
                    let _ = ws
                        .send(Message::Binary(vec![0x20, 3, 0, 0, 0].into()))
                        .await;
                }
                while let Some(Ok(_)) = ws.next().await {}
            });
        }
    });
    (accepted, connects)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_disconnect_during_a_reconnection_handshake_leaves_the_client_disconnected() {
    let addr = unused_address().await;
    let client = MqttClient::with_options(
        ConnectOptions::new("disconnect-during-reconnection")
            .with_reconnect_delay(Duration::from_millis(100), Duration::from_millis(100)),
    );
    client
        .connect(&format!("ws://{addr}/mqtt"))
        .await
        .expect_err("nothing listens there yet");
    let (accepted, connects) = slow_accepting_server(addr).await;
    while accepted.load(Ordering::SeqCst) == 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = client.disconnect().await;
    tokio::time::sleep(HANDSHAKE_DELAY + Duration::from_millis(1500)).await;
    assert!(!client.is_connected().await);
    assert_eq!(connects.load(Ordering::SeqCst), 0);
}
