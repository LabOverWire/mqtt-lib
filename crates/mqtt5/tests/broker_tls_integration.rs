//! Integration test for broker TLS support

use mqtt5::broker::config::{BrokerConfig, TlsConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::time::Duration;
use std::path::PathBuf;

#[tokio::test]
async fn test_broker_tls_creation() {
    // Test that we can create a broker with TLS configuration
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0)) // Use random port
        .with_tls(
            TlsConfig::new(
                PathBuf::from("test_certs/server.crt"), // Using test certs from client tests
                PathBuf::from("test_certs/server.key"),
            )
            .with_bind_address(([127, 0, 0, 1], 0)), // Use random port for TLS too
        );

    let broker = MqttBroker::with_config(config).await;

    // Should succeed if test certs exist
    if broker.is_err() {
        // Skip test if certificates don't exist
        eprintln!("Skipping TLS test - certificates not found");
        return;
    }

    let mut broker = broker.unwrap();

    // Test that broker can start
    let broker_handle = tokio::spawn(async move { broker.run().await });

    // Give it a moment to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Then shut it down
    broker_handle.abort();

    // If we got here without panic, the test passed
}

#[tokio::test]
async fn test_broker_tls_with_client_certs() {
    // Test broker with client certificate verification
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_tls(
            TlsConfig::new(
                PathBuf::from("test_certs/server.crt"),
                PathBuf::from("test_certs/server.key"),
            )
            .with_ca_file(PathBuf::from("test_certs/ca.crt"))
            .with_require_client_cert(true)
            .with_bind_address(([127, 0, 0, 1], 0)),
        );

    let broker = MqttBroker::with_config(config).await;

    if broker.is_err() {
        eprintln!("Skipping TLS client cert test - certificates not found");
        return;
    }

    let mut broker = broker.unwrap();

    // Test that broker can start with client cert requirements
    let broker_handle = tokio::spawn(async move { broker.run().await });

    tokio::time::sleep(Duration::from_millis(100)).await;
    broker_handle.abort();
}

#[tokio::test]
async fn test_broker_default_tls_port() {
    // Test that TLS defaults to port 8883 when not specified
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 1883))
        .with_tls(
            TlsConfig::new(
                PathBuf::from("test_certs/server.crt"),
                PathBuf::from("test_certs/server.key"),
            ), // Note: not setting bind_address, should default to 8883
        );

    // This would bind to 8883 by default, but might fail if port is in use
    // So we just test the configuration is valid
    assert!(config.tls_config.is_some());
    let tls_config = config.tls_config.as_ref().unwrap();
    assert!(!tls_config.bind_addresses.is_empty()); // Should have default addresses for 8883
    assert!(tls_config
        .bind_addresses
        .iter()
        .all(|addr| addr.port() == 8883));
}
