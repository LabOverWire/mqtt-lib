#![cfg(all(feature = "broker", feature = "transport-quic"))]

mod common;

use common::{find_workspace_root, test_ca};
use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, MqttError};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(1);

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
    let addr = broker.quic_local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    (handle, addr)
}

async fn client_holding_a_session_ticket(scheme: &str, addr: SocketAddr) -> MqttClient {
    let client = MqttClient::with_options(
        ConnectOptions::new(format!("early-timeout-{scheme}")).with_automatic_reconnect(false),
    );
    client.set_tls_config(None, None, Some(test_ca())).await;
    client.set_quic_early_data(true).await;
    client.set_quic_connect_timeout(CONNECT_TIMEOUT).await;
    let mut zero_rtt = false;
    for _ in 0..20 {
        client.connect(&format!("{scheme}://{addr}")).await.unwrap();
        zero_rtt = client.was_zero_rtt().await;
        client.disconnect().await.unwrap();
        if zero_rtt {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(zero_rtt, "the client never resumed with early data");
    client
}

async fn assert_early_data_connect_to_a_silent_server_times_out(scheme: &str) {
    let (broker, addr) = start_quic_broker().await;
    let client = client_holding_a_session_ticket(scheme, addr).await;
    broker.abort();
    let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let silent_addr = silent.local_addr().unwrap();
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        client.connect(&format!("{scheme}://{silent_addr}")),
    )
    .await;
    let elapsed = started.elapsed();
    drop(silent);
    let Ok(returned) = outcome else {
        panic!(
            "connect had not returned after {elapsed:?} with a {CONNECT_TIMEOUT:?} connect timeout"
        );
    };
    let Err(error) = returned else {
        panic!("connected to a server that never answers");
    };
    assert!(
        elapsed < CONNECT_TIMEOUT * 3,
        "connect returned {error:?} after {elapsed:?} with a {CONNECT_TIMEOUT:?} connect timeout"
    );
    assert!(
        matches!(error, MqttError::Timeout),
        "expected a timeout, got {error:?}"
    );
}

#[tokio::test]
async fn quics_early_data_connect_honours_connect_timeout() {
    assert_early_data_connect_to_a_silent_server_times_out("quics").await;
}

#[tokio::test]
async fn quic_early_data_connect_honours_connect_timeout() {
    assert_early_data_connect_to_a_silent_server_times_out("quic").await;
}
