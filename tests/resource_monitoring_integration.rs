//! Integration tests for resource monitoring

use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_connection_limits_enforcement() {
    // Create broker with very low connection limit
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(2);

    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let resource_monitor = broker.resource_monitor();
    let _broker_addr = broker.local_addr().expect("Failed to get broker address");

    // Start broker in background
    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    // Give broker time to start
    sleep(Duration::from_millis(100)).await;

    // Test resource monitoring directly
    let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    // Should accept first connection
    assert!(resource_monitor.can_accept_connection(ip).await);
    resource_monitor
        .register_connection("client1".to_string(), ip)
        .await;

    // Should accept second connection
    assert!(resource_monitor.can_accept_connection(ip).await);
    resource_monitor
        .register_connection("client2".to_string(), ip)
        .await;

    // Should reject third connection (exceeds limit)
    assert!(!resource_monitor.can_accept_connection(ip).await);

    // Check statistics
    let stats = resource_monitor.get_stats().await;
    assert_eq!(stats.current_connections, 2);
    assert_eq!(stats.max_connections, 2);
    assert!((stats.connection_utilization() - 100.0).abs() < f64::EPSILON);

    // Clean up
    resource_monitor.unregister_connection("client1", ip).await;
    resource_monitor.unregister_connection("client2", ip).await;

    let final_stats = resource_monitor.get_stats().await;
    assert_eq!(final_stats.current_connections, 0);

    broker_handle.abort();
}

#[tokio::test]
async fn test_per_ip_connection_limits() {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(10);

    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let resource_monitor = broker.resource_monitor();

    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    let ip1 = std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 1));
    let ip2 = std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 2));

    // Connect from first IP - should succeed
    assert!(resource_monitor.can_accept_connection(ip1).await);
    resource_monitor
        .register_connection("client1".to_string(), ip1)
        .await;

    // Connect from second IP - should succeed
    assert!(resource_monitor.can_accept_connection(ip2).await);
    resource_monitor
        .register_connection("client2".to_string(), ip2)
        .await;

    let stats = resource_monitor.get_stats().await;
    assert_eq!(stats.current_connections, 2);
    assert_eq!(stats.unique_ips, 2);

    broker_handle.abort();
}

#[tokio::test]
async fn test_message_rate_limiting() {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(1);

    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let resource_monitor = broker.resource_monitor();

    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    // Register a client
    resource_monitor
        .register_connection("test_client".to_string(), ip)
        .await;

    // Test message rate limiting (default is 1000 messages/sec)
    // Send messages within limit
    for _ in 0..10 {
        assert!(resource_monitor.can_send_message("test_client", 100).await);
    }

    // Check stats were updated
    let stats = resource_monitor.get_stats().await;
    assert_eq!(stats.total_messages, 10);
    assert_eq!(stats.total_bytes, 1000); // 10 messages * 100 bytes each

    broker_handle.abort();
}

#[tokio::test]
async fn test_memory_monitoring() {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(5);

    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let resource_monitor = broker.resource_monitor();

    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    // Test memory usage estimation
    let initial_memory = resource_monitor.get_memory_usage();
    assert_eq!(initial_memory, 0); // No connections initially

    let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    // Add some connections
    for i in 0..3 {
        resource_monitor
            .register_connection(format!("client{i}"), ip)
            .await;
    }

    let memory_with_connections = resource_monitor.get_memory_usage();
    assert!(memory_with_connections > initial_memory);
    assert_eq!(memory_with_connections, 3 * 4096); // 3 connections * 4KB estimate each

    // Test memory limit checking (default is 1GB, should not be exceeded)
    assert!(!resource_monitor.is_memory_limit_exceeded());

    broker_handle.abort();
}
