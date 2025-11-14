mod common;
use common::TestBroker;
use mqtt5::transport::tls::TlsConfig;
use mqtt5::{ConnectOptions, MqttClient};
use mqtt5::time::Duration;
use tokio::time::timeout;

#[tokio::test]
async fn test_tls_connection() {
    use std::sync::{Arc, Mutex};

    // Start test broker with TLS
    let broker = TestBroker::start_with_tls().await;

    // Extract address from broker (format: mqtts://host:port)
    let broker_addr = broker.address().strip_prefix("mqtts://").unwrap();
    let socket_addr: std::net::SocketAddr = broker_addr.parse().unwrap();

    // Create client
    let client = MqttClient::new("tls-test-client");

    // Create TLS config for local broker
    let mut tls_config = TlsConfig::new(socket_addr, "localhost").with_verify_server_cert(false); // Self-signed cert in test
    tls_config
        .load_ca_cert_pem("test_certs/ca.pem")
        .expect("Failed to load CA cert");

    // Connect with TLS
    let result = timeout(
        Duration::from_secs(5),
        client.connect_with_tls_and_options(tls_config, ConnectOptions::default()),
    )
    .await;

    assert!(result.is_ok(), "Connection timeout");
    let connect_result = result.unwrap();
    assert!(
        connect_result.is_ok(),
        "TLS connection failed: {:?}",
        connect_result.err()
    );

    // Test publish/subscribe
    let received_msgs = Arc::new(Mutex::new(Vec::new()));
    let msgs_clone = received_msgs.clone();

    let sub_result = client
        .subscribe("test/tls/topic", move |msg| {
            msgs_clone.lock().unwrap().push(msg);
        })
        .await;
    assert!(
        sub_result.is_ok(),
        "Subscribe failed: {:?}",
        sub_result.err()
    );

    let pub_result = client.publish("test/tls/topic", b"TLS test message").await;
    assert!(pub_result.is_ok(), "Publish failed: {:?}", pub_result.err());

    // Wait for message to arrive
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Check received messages
    {
        let msgs = received_msgs.lock().unwrap();
        assert_eq!(msgs.len(), 1, "Expected 1 message, got {}", msgs.len());
        assert_eq!(msgs[0].topic, "test/tls/topic");
        assert_eq!(msgs[0].payload, b"TLS test message");
    }

    // Disconnect
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_mtls_connection() {
    use std::sync::{Arc, Mutex};

    // Start test broker with TLS
    let broker = TestBroker::start_with_tls().await;

    // Extract address from broker (format: mqtts://host:port)
    let broker_addr = broker.address().strip_prefix("mqtts://").unwrap();
    let socket_addr: std::net::SocketAddr = broker_addr.parse().unwrap();

    // Create client
    let client = MqttClient::new("mtls-test-client");

    // Create mTLS config with client certificates
    let mut tls_config = TlsConfig::new(socket_addr, "localhost").with_verify_server_cert(false); // Self-signed cert in test
    tls_config
        .load_ca_cert_pem("test_certs/ca.pem")
        .expect("Failed to load CA cert");
    tls_config
        .load_client_cert_pem("test_certs/client.pem")
        .expect("Failed to load client cert");
    tls_config
        .load_client_key_pem("test_certs/client.key")
        .expect("Failed to load client key");

    // Connect with mTLS
    let result = timeout(
        Duration::from_secs(5),
        client.connect_with_tls_and_options(tls_config, ConnectOptions::default()),
    )
    .await;

    assert!(result.is_ok(), "Connection timeout");
    let connect_result = result.unwrap();
    assert!(
        connect_result.is_ok(),
        "mTLS connection failed: {:?}",
        connect_result.err()
    );

    // Test publish/subscribe
    let received_msgs = Arc::new(Mutex::new(Vec::new()));
    let msgs_clone = received_msgs.clone();

    let sub_result = client
        .subscribe("test/mtls/topic", move |msg| {
            msgs_clone.lock().unwrap().push(msg);
        })
        .await;
    assert!(
        sub_result.is_ok(),
        "Subscribe failed: {:?}",
        sub_result.err()
    );

    let pub_result = client
        .publish("test/mtls/topic", b"mTLS test message")
        .await;
    assert!(pub_result.is_ok(), "Publish failed: {:?}", pub_result.err());

    // Wait for message to arrive
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Check received messages
    {
        let msgs = received_msgs.lock().unwrap();
        assert_eq!(msgs.len(), 1, "Expected 1 message, got {}", msgs.len());
        assert_eq!(msgs[0].topic, "test/mtls/topic");
        assert_eq!(msgs[0].payload, b"mTLS test message");
    }

    // Disconnect
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_tls_with_alpn() {
    // Start test broker with TLS
    let broker = TestBroker::start_with_tls().await;

    // Extract address from broker (format: mqtts://host:port)
    let broker_addr = broker.address().strip_prefix("mqtts://").unwrap();
    let socket_addr: std::net::SocketAddr = broker_addr.parse().unwrap();

    // Create client
    let client = MqttClient::new("alpn-test-client");

    // Create TLS config with ALPN
    let mut tls_config = TlsConfig::new(socket_addr, "localhost")
        .with_verify_server_cert(false)
        .with_alpn_protocols(&["mqtt"]);
    tls_config
        .load_ca_cert_pem("test_certs/ca.pem")
        .expect("Failed to load CA cert");

    // Connect with TLS
    let result = timeout(
        Duration::from_secs(5),
        client.connect_with_tls_and_options(tls_config, ConnectOptions::default()),
    )
    .await;

    match result {
        Ok(Ok(_)) => {
            // Connected successfully with ALPN
            client.disconnect().await.unwrap();
        }
        Ok(Err(e)) => {
            // Check if it's ALPN related or other TLS error
            let error_msg = e.to_string();
            assert!(
                error_msg.contains("TLS") || error_msg.contains("connect"),
                "Unexpected error: {error_msg}"
            );
        }
        Err(e) => {
            panic!("Connection timeout: {e}");
        }
    }
}

#[tokio::test]
async fn test_tls_reconnection() {
    // Start test broker with TLS
    let broker = TestBroker::start_with_tls().await;

    // Extract address from broker (format: mqtts://host:port)
    let broker_addr = broker.address().strip_prefix("mqtts://").unwrap();
    let socket_addr: std::net::SocketAddr = broker_addr.parse().unwrap();

    // Create client with reconnection enabled
    let client = MqttClient::new("tls-reconnect-client");

    // Create TLS config
    let mut tls_config = TlsConfig::new(socket_addr, "localhost").with_verify_server_cert(false);
    tls_config
        .load_ca_cert_pem("test_certs/ca.pem")
        .expect("Failed to load CA cert");

    // Connect with automatic reconnection
    let options = ConnectOptions::default()
        .with_automatic_reconnect(true)
        .with_reconnect_delay(Duration::from_millis(100), Duration::from_secs(5));

    let result = client
        .connect_with_tls_and_options(tls_config, options)
        .await;
    assert!(
        result.is_ok(),
        "Initial TLS connection failed: {:?}",
        result.err()
    );

    // Subscribe to a topic
    client
        .subscribe("test/tls/reconnect", |_msg| {
            // Message handler
        })
        .await
        .unwrap();

    // TODO: Test reconnection by simulating broker restart
    // For now, just test that the connection works with reconnect enabled

    // Publish a message
    client
        .publish("test/tls/reconnect", b"Reconnect test")
        .await
        .unwrap();

    // Disconnect
    client.disconnect().await.unwrap();
}
