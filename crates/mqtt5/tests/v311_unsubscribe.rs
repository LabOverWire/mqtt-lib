#![cfg(feature = "broker")]

use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, ProtocolVersion};
use std::time::Duration;

#[derive(Debug, PartialEq)]
struct Outcome {
    unsubscribed: bool,
    connected: bool,
    resubscribed: bool,
}

const SERVED: Outcome = Outcome {
    unsubscribed: true,
    connected: true,
    resubscribed: true,
};

async fn unsubscribe(version: ProtocolVersion) -> Outcome {
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory))
        .with_bind_address(([127, 0, 0, 1], 0));
    let mut broker = MqttBroker::with_config(config)
        .await
        .expect("broker starts");
    let addr = broker.local_addr().expect("TCP listener bound");
    let shutdown = broker.shutdown_handle();
    let run = tokio::spawn(async move { broker.run().await });

    let options = ConnectOptions::new("v311-unsubscribe").with_protocol_version(version);
    let client = MqttClient::with_options(options.clone());
    Box::pin(client.connect_with_options(&format!("mqtt://{addr}"), options))
        .await
        .expect("client connects");
    client.subscribe("t", |_| {}).await.expect("subscribe");
    let unsubscribed = tokio::time::timeout(Duration::from_secs(2), client.unsubscribe("t"))
        .await
        .is_ok_and(|result| result.is_ok());
    let connected = client.is_connected().await;
    let resubscribed = tokio::time::timeout(Duration::from_secs(2), client.subscribe("u", |_| {}))
        .await
        .is_ok_and(|result| result.is_ok());

    client.disconnect().await.ok();
    shutdown.shutdown();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("broker stops")
        .expect("broker task joins")
        .expect("broker run succeeds");
    Outcome {
        unsubscribed,
        connected,
        resubscribed,
    }
}

#[tokio::test]
async fn v5_client_unsubscribes_and_stays_connected() {
    assert_eq!(unsubscribe(ProtocolVersion::V5).await, SERVED);
}

#[tokio::test]
async fn v311_client_unsubscribes_and_stays_connected() {
    assert_eq!(unsubscribe(ProtocolVersion::V311).await, SERVED);
}
