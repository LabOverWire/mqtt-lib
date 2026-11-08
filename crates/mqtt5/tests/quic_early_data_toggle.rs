#![cfg(all(feature = "broker", feature = "transport-quic"))]

mod common;

use common::{find_workspace_root, test_ca};
use mqtt5::broker::config::{
    BrokerConfig, QuicConfig as BrokerQuicConfig, StorageBackend, StorageConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::MqttClient;
use std::net::SocketAddr;

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

async fn zero_rtt_after(first_connect_early_data: bool, scheme: &str) -> bool {
    let (broker, addr) = start_quic_broker().await;
    let client = MqttClient::new(format!("toggle-{first_connect_early_data}-{scheme}"));
    client.set_tls_config(None, None, Some(test_ca())).await;
    client.set_quic_early_data(first_connect_early_data).await;
    client.connect(&format!("{scheme}://{addr}")).await.unwrap();
    client.disconnect().await.unwrap();
    client.set_quic_early_data(true).await;
    let mut zero_rtt = false;
    for _ in 0..20 {
        client.connect(&format!("{scheme}://{addr}")).await.unwrap();
        zero_rtt = client.was_zero_rtt().await;
        client.disconnect().await.unwrap();
        if zero_rtt {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    broker.abort();
    zero_rtt
}

#[tokio::test]
async fn control_early_data_on_from_the_start_uses_zero_rtt_quics() {
    assert!(zero_rtt_after(true, "quics").await);
}

#[tokio::test]
async fn early_data_turned_on_after_a_connect_uses_zero_rtt_quics() {
    assert!(zero_rtt_after(false, "quics").await);
}

#[tokio::test]
async fn control_early_data_on_from_the_start_uses_zero_rtt_quic() {
    assert!(zero_rtt_after(true, "quic").await);
}

#[tokio::test]
async fn early_data_turned_on_after_a_connect_uses_zero_rtt_quic() {
    assert!(zero_rtt_after(false, "quic").await);
}
