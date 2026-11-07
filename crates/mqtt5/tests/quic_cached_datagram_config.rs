#![cfg(feature = "broker")]
#![cfg(feature = "transport-quic")]

use mqtt5::broker::quic_acceptor::QuicAcceptorConfig;
use mqtt5::{MqttClient, PublishOptions, QoS};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const CERT: &str = "../../test_certs/server.pem";
const KEY: &str = "../../test_certs/server.key";
const CONNACK: [u8; 5] = [0x20, 0x03, 0x00, 0x00, 0x00];

async fn second_connection_datagrams(early_data: bool, first: bool) -> (bool, usize) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certs = QuicAcceptorConfig::load_cert_chain_from_file(CERT)
        .await
        .unwrap();
    let key = QuicAcceptorConfig::load_private_key_from_file(KEY)
        .await
        .unwrap();
    let server_config = QuicAcceptorConfig::new(certs, key)
        .build_server_config()
        .unwrap();
    let endpoint = quinn::Endpoint::server(server_config, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = endpoint.local_addr().unwrap();
    let datagrams = Arc::new(AtomicUsize::new(0));
    let server_datagrams = Arc::clone(&datagrams);

    let server = tokio::spawn(async move {
        let mut supported = Vec::new();
        let mut held = Vec::new();
        for _ in 0..2 {
            let connection = endpoint.accept().await.unwrap().await.unwrap();
            supported.push(connection.max_datagram_size().is_some());
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = recv.read(&mut buf).await.unwrap();
            send.write_all(&CONNACK).await.unwrap();
            let reader = connection.clone();
            let connection_datagrams = Arc::clone(&server_datagrams);
            tokio::spawn(async move {
                while reader.read_datagram().await.is_ok() {
                    connection_datagrams.fetch_add(1, Ordering::SeqCst);
                }
            });
            held.push((connection, send, recv));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
        (supported, held, endpoint)
    });

    let client = MqttClient::new("cached-cfg");
    client.set_insecure_tls(true).await;
    client.set_quic_early_data(early_data).await;
    client.set_quic_datagrams(first).await;
    client.connect(&format!("quic://{addr}")).await.unwrap();
    client.disconnect().await.unwrap();

    client.set_quic_datagrams(!first).await;
    client.connect(&format!("quic://{addr}")).await.unwrap();
    client
        .publish_with_options(
            "t",
            b"x".to_vec(),
            PublishOptions {
                qos: QoS::AtMostOnce,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let (supported, held, endpoint) = server.await.unwrap();
    drop(held);
    endpoint.close(0u32.into(), b"done");
    (supported[1], datagrams.load(Ordering::SeqCst))
}

#[tokio::test]
async fn datagrams_enabled_after_reconnect_without_early_data() {
    let result = second_connection_datagrams(false, false).await;
    println!(
        "early_data=false: second connection advertises={} datagrams_received={}",
        result.0, result.1
    );
    assert_eq!(result, (true, 1));
}

#[tokio::test]
async fn datagrams_enabled_after_reconnect_with_early_data() {
    let result = second_connection_datagrams(true, false).await;
    println!(
        "early_data=true: second connection advertises={} datagrams_received={}",
        result.0, result.1
    );
    assert_eq!(result, (true, 1));
}

#[tokio::test]
async fn datagrams_disabled_after_reconnect_with_early_data() {
    let result = second_connection_datagrams(true, true).await;
    println!(
        "early_data=true on->off: second connection advertises={} datagrams_received={}",
        result.0, result.1
    );
    assert_eq!(result, (false, 0));
}
