#![cfg(feature = "broker")]

use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn test_connection_limits_enforcement() {
    let config = BrokerConfig::default()
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(2);

    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let resource_monitor = broker.resource_monitor();
    let _broker_addr = broker.local_addr().expect("Failed to get broker address");

    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    assert!(resource_monitor.can_accept_connection(ip).await);
    resource_monitor
        .register_connection("client1".to_string(), ip)
        .await;

    assert!(resource_monitor.can_accept_connection(ip).await);
    resource_monitor
        .register_connection("client2".to_string(), ip)
        .await;

    assert!(!resource_monitor.can_accept_connection(ip).await);

    let stats = resource_monitor.get_stats().await;
    assert_eq!(stats.current_connections, 2);
    assert_eq!(stats.max_connections, 2);
    assert!((stats.connection_utilization() - 100.0).abs() < f64::EPSILON);

    resource_monitor.unregister_connection("client1", ip).await;
    resource_monitor.unregister_connection("client2", ip).await;

    let final_stats = resource_monitor.get_stats().await;
    assert_eq!(final_stats.current_connections, 0);

    broker_handle.abort();
}

#[tokio::test]
async fn test_per_ip_connection_limits() {
    let config = BrokerConfig::default()
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
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

    assert!(resource_monitor.can_accept_connection(ip1).await);
    resource_monitor
        .register_connection("client1".to_string(), ip1)
        .await;

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
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
        .with_bind_address("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .with_max_clients(1);

    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let resource_monitor = broker.resource_monitor();

    let broker_handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });

    sleep(Duration::from_millis(100)).await;

    let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);

    resource_monitor
        .register_connection("test_client".to_string(), ip)
        .await;

    for _ in 0..10 {
        assert!(resource_monitor.can_send_message("test_client", 100).await);
    }

    let stats = resource_monitor.get_stats().await;
    assert_eq!(stats.total_messages, 10);
    assert_eq!(stats.total_bytes, 1000);

    broker_handle.abort();
}
