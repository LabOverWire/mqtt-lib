mod common;
use common::TestBroker;
use mqtt5::transport::tls::TlsConfig;
use mqtt5::validation::{RestrictiveValidator, StandardValidator, TopicValidator};
use mqtt5::{ConnectOptions, MqttClient};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use mqtt5::time::Duration;
use tokio::time::sleep;

// For AWS IoT validator, import from the submodule
use mqtt5::validation::namespace::NamespaceValidator;

#[tokio::test]
async fn test_direct_tls_config_with_alpn() {
    // Initialize rustls crypto provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Start test broker with TLS
    let broker = TestBroker::start_with_tls().await;

    // Extract address from broker
    let broker_addr = broker.address().strip_prefix("mqtts://").unwrap();
    let addr: std::net::SocketAddr = broker_addr.parse().unwrap();

    let client = MqttClient::new("direct-tls-alpn-test");

    // Create TLS config with ALPN - use standard mqtt protocol
    let mut tls_config = TlsConfig::new(addr, "localhost")
        .with_alpn_protocols(&["mqtt"]) // Use standard MQTT ALPN
        .with_verify_server_cert(false);

    // Load test certificates
    tls_config
        .load_ca_cert_pem("test_certs/ca.pem")
        .expect("Failed to load CA cert");

    // Connect and verify it works
    client
        .connect_with_tls(tls_config)
        .await
        .expect("Failed to connect with ALPN TLS config");

    // Test that we can publish/subscribe
    let received = Arc::new(AtomicBool::new(false));
    let received_clone = received.clone();

    client
        .subscribe("test/alpn", move |_| {
            received_clone.store(true, Ordering::SeqCst);
        })
        .await
        .expect("Failed to subscribe");

    client
        .publish("test/alpn", b"ALPN test")
        .await
        .expect("Failed to publish");

    sleep(Duration::from_millis(100)).await;
    assert!(
        received.load(Ordering::SeqCst),
        "Message not received via ALPN TLS connection"
    );

    client.disconnect().await.expect("Failed to disconnect");
}

#[tokio::test]
async fn test_tls_config_with_custom_alpn() {
    // Initialize rustls crypto provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Start test broker with TLS
    let broker = TestBroker::start_with_tls().await;

    // Extract address from broker
    let broker_addr = broker.address().strip_prefix("mqtts://").unwrap();
    let addr: std::net::SocketAddr = broker_addr.parse().unwrap();

    let client = MqttClient::new("custom-alpn-test");

    // Test custom ALPN protocols
    let mut tls_config = TlsConfig::new(addr, "localhost")
        .with_alpn_protocols(&["mqtt"])
        .with_verify_server_cert(false);

    // Load test certificates
    tls_config
        .load_ca_cert_pem("test_certs/ca.pem")
        .expect("Failed to load CA cert");

    // Connect and verify it works
    client
        .connect_with_tls(tls_config)
        .await
        .expect("Failed to connect with custom ALPN");

    // Quick connectivity test
    client
        .publish("test/custom-alpn", b"Custom ALPN test")
        .await
        .expect("Failed to publish with custom ALPN");

    client.disconnect().await.expect("Failed to disconnect");
}

#[tokio::test]
async fn test_tls_config_with_connect_options() {
    // Initialize rustls crypto provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Start test broker with TLS
    let broker = TestBroker::start_with_tls().await;

    // Extract address from broker
    let broker_addr = broker.address().strip_prefix("mqtts://").unwrap();
    let addr: std::net::SocketAddr = broker_addr.parse().unwrap();

    // Test combining TLS config with MQTT options
    let mut options = ConnectOptions::new("tls-options-test")
        .with_clean_start(true)
        .with_keep_alive(Duration::from_secs(30));

    // Set custom packet size limit
    options.properties.maximum_packet_size = Some(131_072); // 128KB

    let mut tls_config = TlsConfig::new(addr, "localhost").with_verify_server_cert(false);

    // Load test certificates
    tls_config
        .load_ca_cert_pem("test_certs/ca.pem")
        .expect("Failed to load CA cert");

    let client = MqttClient::with_options(options.clone());
    client
        .connect_with_tls_and_options(tls_config, options)
        .await
        .expect("Failed to connect with TLS and custom options");

    // Verify connection works with custom options
    let received = Arc::new(AtomicBool::new(false));
    let received_clone = received.clone();

    client
        .subscribe("test/options", move |_| {
            received_clone.store(true, Ordering::SeqCst);
        })
        .await
        .expect("Failed to subscribe");

    // Test that packet size limit is respected
    let large_payload = vec![0u8; 1024]; // Well within limit
    client
        .publish("test/options", large_payload.as_slice())
        .await
        .expect("Failed to publish within size limit");

    sleep(Duration::from_millis(100)).await;
    assert!(
        received.load(Ordering::SeqCst),
        "Message not received with custom options"
    );

    client.disconnect().await.expect("Failed to disconnect");
}

#[test]
fn test_topic_validators() {
    // Test standard validator
    let standard = StandardValidator;
    assert!(standard.validate_topic_name("sensor/temperature").is_ok());
    assert!(standard.validate_topic_filter("sensor/+").is_ok());
    assert!(!standard.is_reserved_topic("any/topic"));

    // Test restrictive validator
    let restrictive = RestrictiveValidator::new()
        .with_reserved_prefix("internal/")
        .with_max_levels(3)
        .with_max_topic_length(50);

    assert!(restrictive
        .validate_topic_name("public/sensor/temp")
        .is_ok());
    assert!(restrictive.validate_topic_name("internal/secret").is_err());
    assert!(restrictive.validate_topic_name("a/b/c/d").is_err()); // Too many levels
    assert!(restrictive.is_reserved_topic("internal/anything"));

    // Test AWS IoT validator
    let aws_validator = NamespaceValidator::aws_iot().with_device_id("my-device");

    // Device-specific topics should work
    assert!(aws_validator
        .validate_topic_name("$aws/things/my-device/shadow/update")
        .is_ok());

    // Other device topics should fail
    assert!(aws_validator
        .validate_topic_name("$aws/things/other-device/shadow/update")
        .is_err());

    // System topics should fail by default
    assert!(aws_validator
        .validate_topic_name("$SYS/broker/version")
        .is_err());
    assert!(aws_validator.is_reserved_topic("$SYS/broker/version"));

    // Regular topics should work
    assert!(aws_validator
        .validate_topic_name("sensor/temperature")
        .is_ok());
    assert!(!aws_validator.is_reserved_topic("sensor/temperature"));
}

#[test]
fn test_aws_iot_validator_device_specific() {
    let validator = NamespaceValidator::aws_iot().with_device_id("sensor-001");

    // Allowed device topics
    assert!(validator
        .validate_topic_name("$aws/things/sensor-001/shadow/update")
        .is_ok());
    assert!(validator
        .validate_topic_name("$aws/things/sensor-001/jobs/get")
        .is_ok());

    // Rejected device topics
    assert!(validator
        .validate_topic_name("$aws/things/sensor-002/shadow/update")
        .is_err());
    assert!(validator.is_reserved_topic("$aws/things/sensor-002/shadow/update"));

    // General AWS topics should be allowed
    assert!(validator
        .validate_topic_name("$aws/events/presence/connected/sensor-001")
        .is_ok());
}

#[test]
fn test_aws_iot_validator_system_topics() {
    let validator = NamespaceValidator::new("$aws", "thing").with_system_topics(true);

    // System topics should now be allowed
    assert!(validator.validate_topic_name("$SYS/broker/version").is_ok());
    assert!(validator.validate_topic_name("$SYS/broker/uptime").is_ok());
    assert!(!validator.is_reserved_topic("$SYS/broker/version"));
}

#[test]
fn test_restrictive_validator_features() {
    let validator = RestrictiveValidator::new()
        .with_reserved_prefix("company/")
        .with_reserved_prefix("internal/")
        .with_max_levels(4)
        .with_max_topic_length(30)
        .with_prohibited_char(' ')
        .with_prohibited_char('\t');

    // Test reserved prefixes
    assert!(validator.validate_topic_name("company/secret").is_err());
    assert!(validator.validate_topic_name("internal/admin").is_err());
    assert!(validator.is_reserved_topic("company/anything"));

    // Test max levels
    assert!(validator.validate_topic_name("a/b/c/d").is_ok()); // 4 levels OK
    assert!(validator.validate_topic_name("a/b/c/d/e").is_err()); // 5 levels too many

    // Test max length
    assert!(validator.validate_topic_name("short").is_ok());
    assert!(validator
        .validate_topic_name("this_is_a_very_long_topic_name_that_exceeds_limit")
        .is_err());

    // Test prohibited characters
    assert!(validator.validate_topic_name("topic with space").is_err());
    assert!(validator.validate_topic_name("topic\twith\ttab").is_err());
    assert!(validator.validate_topic_name("normal/topic").is_ok());
}

#[test]
fn test_alpn_protocol_validation() {
    // Test valid ALPN protocols
    let config = TlsConfig::new("127.0.0.1:8883".parse().unwrap(), "localhost")
        .with_alpn_protocols(&["mqtt", "http/1.1"]);

    // Should not panic
    drop(config);

    // Test AWS IoT convenience method
    let aws_config =
        TlsConfig::new("127.0.0.1:443".parse().unwrap(), "localhost").with_aws_iot_alpn();

    drop(aws_config);
}

#[test]
#[should_panic(expected = "ALPN protocol cannot be empty")]
fn test_alpn_empty_protocol_validation() {
    // This will panic due to empty protocol validation
    let _ = TlsConfig::new("127.0.0.1:8883".parse().unwrap(), "localhost")
        .with_alpn_protocols(&["mqtt", ""]);
}

#[test]
#[should_panic(expected = "ALPN protocol cannot exceed 255 bytes")]
fn test_alpn_long_protocol_validation() {
    let long_protocol = "a".repeat(256);
    // This will panic due to protocol length validation
    let _ = TlsConfig::new("127.0.0.1:8883".parse().unwrap(), "localhost")
        .with_alpn_protocols(&[&long_protocol]);
}
