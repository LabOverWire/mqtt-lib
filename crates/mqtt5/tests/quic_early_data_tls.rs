//! With early data on, a client keeps the QUIC configuration of its last
//! connection to use again: it is used only with the TLS settings it was
//! built with, so a quics:// connection is verified as those settings say.
#![cfg(all(feature = "broker", feature = "transport-quic"))]

mod common;

use common::{find_workspace_root, other_ca, test_ca};
use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::MqttClient;
use std::net::SocketAddr;

/// A QUIC broker with early data on, whose certificate is signed by the
/// test CA: trusted by no system roots
async fn start_quic_broker() -> (tokio::task::JoinHandle<()>, SocketAddr) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certs = find_workspace_root().join("test_certs");
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            BrokerQuicConfig::new(certs.join("server.pem"), certs.join("server.key"))
                .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
                .with_early_data(true),
        );
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker
        .quic_local_addr()
        .expect("QUIC endpoint must be bound");
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    (handle, addr)
}

#[tokio::test]
async fn quics_after_quic_is_verified() {
    let (broker, addr) = start_quic_broker().await;
    let client = MqttClient::new("early-data-after-quic");
    client.set_quic_early_data(true).await;
    client
        .connect(&format!("quic://{addr}"))
        .await
        .expect("quic:// verifies nothing");
    client.disconnect().await.unwrap();
    client
        .connect(&format!("quics://{addr}"))
        .await
        .expect_err("the test CA is not among the system roots");
    broker.abort();
}

#[tokio::test]
async fn quics_after_insecure_tls_is_verified() {
    let (broker, addr) = start_quic_broker().await;
    let client = MqttClient::new("early-data-after-insecure");
    client.set_quic_early_data(true).await;
    client.set_insecure_tls(true).await;
    client
        .connect(&format!("quics://{addr}"))
        .await
        .expect("no verification asked");
    client.disconnect().await.unwrap();
    client.set_insecure_tls(false).await;
    client
        .connect(&format!("quics://{addr}"))
        .await
        .expect_err("the test CA is not among the system roots");
    broker.abort();
}

#[tokio::test]
async fn quics_takes_a_new_stored_tls_config() {
    let (broker, addr) = start_quic_broker().await;
    let client = MqttClient::new("early-data-new-stored");
    client.set_quic_early_data(true).await;
    client.set_tls_config(None, None, Some(test_ca())).await;
    client
        .connect(&format!("quics://{addr}"))
        .await
        .expect("trusted by the stored configuration's CA");
    client.disconnect().await.unwrap();
    client.set_tls_config(None, None, Some(other_ca())).await;
    client
        .connect(&format!("quics://{addr}"))
        .await
        .expect_err("not trusted by the CA stored since");
    broker.abort();
}

#[tokio::test]
async fn early_data_is_used_again_with_the_same_settings() {
    let (broker, addr) = start_quic_broker().await;
    let client = MqttClient::new("early-data-kept");
    client.set_quic_early_data(true).await;
    client.set_tls_config(None, None, Some(test_ca())).await;
    client
        .connect(&format!("quics://{addr}"))
        .await
        .expect("trusted by the stored configuration's CA");
    client.disconnect().await.unwrap();
    // The server sends its session ticket after the handshake: the second
    // connection uses 0-RTT once it has arrived.
    let mut zero_rtt = false;
    for _ in 0..20 {
        client
            .connect(&format!("quics://{addr}"))
            .await
            .expect("trusted again");
        zero_rtt = client.was_zero_rtt().await;
        client.disconnect().await.unwrap();
        if zero_rtt {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(zero_rtt, "a later connection uses 0-RTT");
    broker.abort();
}

/// What a QUIC server learns of the client that connects to it: the protocol
/// negotiated, from `MQTT-next` and `mqtt` in that order, and whether it may
/// open a second stream of its own while the first stays open
struct ClientSeen {
    protocol: Vec<u8>,
    second_stream_opened: bool,
}

/// A QUIC server with the broker's certificate that takes one connection,
/// says what it learned of the client, and answers nothing
fn stand_in_server() -> (SocketAddr, tokio::sync::oneshot::Receiver<ClientSeen>) {
    use rustls_pki_types::pem::PemObject;
    use rustls_pki_types::{CertificateDer, PrivateKeyDer};
    let certs = find_workspace_root().join("test_certs");
    let chain = CertificateDer::pem_file_iter(certs.join("server.pem"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_file(certs.join("server.key")).unwrap();
    let mut tls = rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(chain, key)
    .unwrap();
    tls.alpn_protocols = vec![b"MQTT-next".to_vec(), b"mqtt".to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls).unwrap();
    let endpoint = quinn::Endpoint::server(
        quinn::ServerConfig::with_crypto(std::sync::Arc::new(crypto)),
        "127.0.0.1:0".parse().unwrap(),
    )
    .unwrap();
    let addr = endpoint.local_addr().unwrap();
    let (tell, seen) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let connection = endpoint.accept().await.unwrap().await.unwrap();
        let protocol = connection
            .handshake_data()
            .and_then(|data| data.downcast::<quinn::crypto::rustls::HandshakeData>().ok())
            .and_then(|data| data.protocol)
            .unwrap_or_default();
        let _first = connection.open_uni().await.unwrap();
        let second_stream_opened =
            tokio::time::timeout(std::time::Duration::from_millis(300), connection.open_uni())
                .await
                .is_ok();
        let _ = tell.send(ClientSeen {
            protocol,
            second_stream_opened,
        });
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        drop(endpoint);
    });
    (addr, seen)
}

/// A client with early data on and the test CA stored, connected once to the
/// broker with the QUIC settings it was made with, so that it keeps that
/// connection's configuration
async fn client_that_kept_a_configuration(id: &str) -> (MqttClient, tokio::task::JoinHandle<()>) {
    let (broker, addr) = start_quic_broker().await;
    let client = MqttClient::new(id);
    client.set_quic_early_data(true).await;
    client
        .set_quic_connect_timeout(std::time::Duration::from_secs(1))
        .await;
    client.set_tls_config(None, None, Some(test_ca())).await;
    client
        .connect(&format!("quics://{addr}"))
        .await
        .expect("trusted by the stored configuration's CA");
    client.disconnect().await.unwrap();
    (client, broker)
}

#[tokio::test]
async fn flow_headers_turned_on_since_are_offered() {
    let (client, broker) = client_that_kept_a_configuration("early-data-flow-headers").await;
    client.set_quic_flow_headers(true).await;
    let (addr, seen) = stand_in_server();
    let _ = client.connect(&format!("quics://{addr}")).await;
    let seen = seen.await.expect("the client reached the server");
    assert_eq!(
        String::from_utf8_lossy(&seen.protocol),
        "MQTT-next",
        "a fresh connection with flow headers offers MQTT-next"
    );
    broker.abort();
}

#[tokio::test]
async fn a_stream_limit_set_since_is_given_to_the_server() {
    let (client, broker) = client_that_kept_a_configuration("early-data-stream-limit").await;
    client.set_quic_max_streams(Some(1)).await;
    let (addr, seen) = stand_in_server();
    let _ = client.connect(&format!("quics://{addr}")).await;
    let seen = seen.await.expect("the client reached the server");
    assert!(
        !seen.second_stream_opened,
        "the server may open only one stream at a time"
    );
    broker.abort();
}
