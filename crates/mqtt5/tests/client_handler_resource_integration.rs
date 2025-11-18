//! Integration tests for `ClientHandler` with `ResourceMonitor`

mod common;
use mqtt5::broker::config::{StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::client::MqttClient;
use mqtt5::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_real_client_connection_limits() {
    // Create broker with very low connection limit and in-memory storage
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(2)
        .with_storage(storage_config);

    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let resource_monitor = broker.resource_monitor();
    let broker_addr = broker.local_addr().expect("Failed to get broker address");

    // Start broker in background
    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    // Give broker time to start
    sleep(Duration::from_millis(100)).await;

    // Try to connect multiple clients
    let mut clients = Vec::new();

    // First two clients should connect successfully
    for i in 0..2 {
        let client_id = format!("client-{i}");
        let client = MqttClient::new(&client_id);

        match client.connect(&broker_addr.to_string()).await {
            Ok(()) => {
                println!("Client {client_id} connected successfully");
                clients.push(client);
            }
            Err(e) => {
                panic!("Client {client_id} should have connected but failed: {e}");
            }
        }

        sleep(Duration::from_millis(50)).await;
    }

    // Check resource statistics
    let stats = resource_monitor.get_stats().await;
    assert_eq!(stats.current_connections, 2);
    assert!((stats.connection_utilization() - 100.0).abs() < f64::EPSILON);

    // Third client should fail to connect due to limits
    let third_client = MqttClient::new("client-3");
    let result = third_client.connect(&broker_addr.to_string()).await;

    // The connection should be rejected at TCP level, so we expect a connection error
    assert!(
        result.is_err(),
        "Third client should not be able to connect"
    );

    // Statistics should remain at 2 connections
    let final_stats = resource_monitor.get_stats().await;
    assert_eq!(final_stats.current_connections, 2);

    // Clean up
    for client in clients {
        let _ = client.disconnect().await;
    }

    broker_handle.abort();
}

#[tokio::test]
async fn test_real_client_rate_limiting() {
    // Create broker with very low message rate limits for testing
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(5)
        .with_storage(storage_config);

    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let resource_monitor = broker.resource_monitor();
    let broker_addr = broker.local_addr().expect("Failed to get broker address");

    // Start broker in background
    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    // Connect a client
    let client = MqttClient::new("rate-test-client");
    client.connect(&broker_addr.to_string()).await.unwrap();

    // Verify client is registered with resource monitor
    let initial_stats = resource_monitor.get_stats().await;
    assert_eq!(initial_stats.current_connections, 1);

    // Publish messages rapidly (testing internal rate limiting)
    let mut success_count = 0;
    let mut error_count = 0;

    for i in 0..20 {
        match client
            .publish(&format!("test/message/{i}"), "test payload")
            .await
        {
            Ok(_) => {
                success_count += 1;
            }
            Err(_) => {
                error_count += 1;
            }
        }

        // Small delay to avoid overwhelming
        sleep(Duration::from_millis(1)).await;
    }

    println!("Published {success_count} messages successfully, {error_count} failed");

    // Check that messages were tracked in resource monitor
    let final_stats = resource_monitor.get_stats().await;
    assert!(
        final_stats.total_messages > 0,
        "Some messages should have been tracked"
    );
    assert!(
        final_stats.total_bytes > 0,
        "Some bytes should have been tracked"
    );

    // Most messages should succeed since default rate limits are high (1000 msg/sec)
    // This test mainly verifies the integration is working
    assert!(
        success_count > 10,
        "Most messages should succeed with default limits"
    );

    // Clean up
    let _ = client.disconnect().await;
    broker_handle.abort();
}

#[tokio::test]
async fn test_client_registration_unregistration() {
    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(10)
        .with_storage(storage_config);

    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let resource_monitor = broker.resource_monitor();
    let broker_addr = broker.local_addr().expect("Failed to get broker address");

    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    // Initially no connections
    let initial_stats = resource_monitor.get_stats().await;
    assert_eq!(initial_stats.current_connections, 0);

    // Connect client and verify registration
    let client = MqttClient::new("reg-test-client");
    client.connect(&broker_addr.to_string()).await.unwrap();

    sleep(Duration::from_millis(50)).await;

    let connected_stats = resource_monitor.get_stats().await;
    assert_eq!(connected_stats.current_connections, 1);
    assert_eq!(connected_stats.unique_ips, 1);

    // Disconnect client and verify unregistration
    client.disconnect().await.unwrap();

    sleep(Duration::from_millis(50)).await;

    let disconnected_stats = resource_monitor.get_stats().await;
    assert_eq!(disconnected_stats.current_connections, 0);

    broker_handle.abort();
}
