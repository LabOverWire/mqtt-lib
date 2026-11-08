#![cfg(all(feature = "transport-websocket", feature = "transport-quic"))]

use futures_util::{SinkExt, StreamExt};
use mqtt5::{ConnectOptions, MqttClient};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::handshake::server::{
    Callback, ErrorResponse, Request, Response,
};
use tokio_tungstenite::tungstenite::Message;

const ACCEPTED: [u8; 5] = [0x20, 3, 0, 0, 0];

#[derive(Clone)]
enum Answer {
    Close,
    Reply(Vec<u8>),
}

fn options(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_reconnect_delay(Duration::from_millis(1100), Duration::from_millis(1100))
}

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

fn server_cert() -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    let chain = CertificateDer::pem_file_iter(certs().join("server.pem"))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let key = PrivateKeyDer::from_pem_file(certs().join("server.key")).unwrap();
    (chain, key)
}

struct Scripted {
    url: String,
    answer: Arc<Mutex<Answer>>,
    cut: tokio::sync::watch::Sender<bool>,
}

struct OfferingMqtt;

impl Callback for OfferingMqtt {
    fn on_request(self, _: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "mqtt".parse().unwrap());
        Ok(response)
    }
}

async fn ws_server() -> Scripted {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let url = format!("ws://{addr}/mqtt");
    let answer = Arc::new(Mutex::new(Answer::Reply(ACCEPTED.to_vec())));
    let (cut, cut_rx) = tokio::sync::watch::channel(false);
    let (counted, answering) = (Arc::new(AtomicUsize::new(0)), answer.clone());
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let first = counted.fetch_add(1, Ordering::SeqCst) == 0;
            let reply = answering.lock().unwrap().clone();
            let mut cut = cut_rx.clone();
            tokio::spawn(async move {
                let mut ws = tokio_tungstenite::accept_hdr_async(stream, OfferingMqtt)
                    .await
                    .unwrap();
                let _ = ws.next().await;
                match reply {
                    Answer::Close => {
                        let _ = ws.close(None).await;
                    }
                    Answer::Reply(bytes) => {
                        let _ = ws.send(Message::Binary(bytes.into())).await;
                        if first {
                            let _ = cut.wait_for(|cut| *cut).await;
                            return;
                        }
                        while let Some(Ok(_)) = ws.next().await {}
                    }
                }
            });
        }
    });
    Scripted { url, answer, cut }
}

fn quic_server() -> Scripted {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (chain, key) = server_cert();
    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .unwrap();
    crypto.alpn_protocols = vec![b"mqtt".to_vec()];
    let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(crypto).unwrap();
    let config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
    let endpoint = quinn::Endpoint::server(config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();
    let url = format!("quic://{addr}");
    let answer = Arc::new(Mutex::new(Answer::Reply(ACCEPTED.to_vec())));
    let (cut, cut_rx) = tokio::sync::watch::channel(false);
    let (counted, answering) = (Arc::new(AtomicUsize::new(0)), answer.clone());
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let first = counted.fetch_add(1, Ordering::SeqCst) == 0;
            let reply = answering.lock().unwrap().clone();
            let mut cut = cut_rx.clone();
            tokio::spawn(async move {
                let Ok(connection) = incoming.await else {
                    return;
                };
                let Ok((mut send, mut recv)) = connection.accept_bi().await else {
                    return;
                };
                let mut buffer = [0u8; 2048];
                let _ = recv.read(&mut buffer).await;
                match reply {
                    Answer::Close => {
                        let _ = send.finish();
                        tokio::time::sleep(Duration::from_secs(20)).await;
                    }
                    Answer::Reply(bytes) => {
                        let _ = send.write_all(&bytes).await;
                        if first {
                            let _ = cut.wait_for(|cut| *cut).await;
                            connection.close(0u32.into(), b"cut");
                            return;
                        }
                        tokio::time::sleep(Duration::from_secs(20)).await;
                    }
                }
            });
        }
    });
    Scripted { url, answer, cut }
}

async fn lost(server: &Scripted, id: &str) -> (MqttClient, ConnectOptions) {
    let retrying = options(id);
    let client = MqttClient::with_options(retrying.clone());
    client
        .connect_with_options(&server.url, retrying.clone())
        .await
        .unwrap();
    server.cut.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!client.is_connected().await);
    (client, retrying)
}

async fn closes_before_connack_keeps_reconnecting(server: Scripted, label: &str) {
    let (client, retrying) = lost(&server, label).await;
    *server.answer.lock().unwrap() = Answer::Close;
    assert!(client
        .connect_with_options(&server.url, retrying)
        .await
        .is_err());
    *server.answer.lock().unwrap() = Answer::Reply(ACCEPTED.to_vec());
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(client.is_connected().await, "{label}");
}

#[tokio::test]
async fn a_websocket_closed_before_connack_keeps_reconnecting() {
    closes_before_connack_keeps_reconnecting(ws_server().await, "ws").await;
}

#[tokio::test]
async fn a_quic_stream_finished_before_connack_keeps_reconnecting() {
    closes_before_connack_keeps_reconnecting(quic_server(), "quic").await;
}
