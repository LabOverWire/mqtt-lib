mod common;

use common::cli_helpers::*;
use common::TestBroker;
use mqtt5::broker::config::{BrokerConfig, LoadBalancerConfig, StorageBackend, StorageConfig};
use mqtt5::time::Duration;
use std::net::SocketAddr;

fn memory_storage() -> StorageConfig {
    StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    }
}

fn lb_config(backends: Vec<String>) -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(memory_storage())
        .with_load_balancer(LoadBalancerConfig::new(backends))
}

fn backend_config() -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(memory_storage())
}

#[tokio::test]
async fn test_cli_pub_through_lb_redirect() {
    let backend = TestBroker::start_with_config(backend_config()).await;
    let lb = TestBroker::start_with_config(lb_config(vec![backend.address().to_string()])).await;

    let result = run_cli_pub(lb.address(), "test/lb/pub", "redirected-message", &[]).await;
    assert!(
        result.success,
        "CLI pub through LB should succeed: {}",
        result.stderr
    );

    drop(lb);
    drop(backend);
}

#[tokio::test]
async fn test_cli_pub_sub_through_lb_redirect() {
    let backend = TestBroker::start_with_config(backend_config()).await;
    let lb = TestBroker::start_with_config(lb_config(vec![backend.address().to_string()])).await;

    let sub_handle = run_cli_sub_async(backend.address(), "test/lb/delivery", 1, &[]);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let pub_result = run_cli_pub(lb.address(), "test/lb/delivery", "via-lb", &[]).await;
    assert!(
        pub_result.success,
        "Publish through LB failed: {}",
        pub_result.stderr
    );

    let sub_result = tokio::time::timeout(Duration::from_secs(3), sub_handle)
        .await
        .expect("Timeout waiting for subscriber")
        .expect("Subscriber task failed");

    assert!(
        sub_result.stdout_contains("via-lb"),
        "Message should be delivered through backend. stdout: {}, stderr: {}",
        sub_result.stdout,
        sub_result.stderr
    );

    drop(lb);
    drop(backend);
}

#[tokio::test]
async fn test_cli_pub_to_dead_backend_via_lb() {
    let lb = TestBroker::start_with_config(lb_config(vec!["mqtt://127.0.0.1:1".to_string()])).await;

    let result = run_cli_pub(lb.address(), "test/lb/dead", "should-fail", &[]).await;
    assert!(!result.success, "CLI pub to dead backend should fail");

    drop(lb);
}

#[tokio::test]
async fn test_cli_sub_through_lb_redirect() {
    let backend = TestBroker::start_with_config(backend_config()).await;
    let lb = TestBroker::start_with_config(lb_config(vec![backend.address().to_string()])).await;

    let sub_handle = run_cli_sub_async(lb.address(), "test/lb/sub", 1, &[]);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let pub_result = run_cli_pub(backend.address(), "test/lb/sub", "sub-via-lb", &[]).await;
    assert!(
        pub_result.success,
        "Direct publish to backend failed: {}",
        pub_result.stderr
    );

    let sub_result = tokio::time::timeout(Duration::from_secs(3), sub_handle)
        .await
        .expect("Timeout waiting for subscriber")
        .expect("Subscriber task failed");

    assert!(
        sub_result.stdout_contains("sub-via-lb"),
        "Subscriber through LB should receive message. stdout: {}, stderr: {}",
        sub_result.stdout,
        sub_result.stderr
    );

    drop(lb);
    drop(backend);
}

#[tokio::test]
async fn test_cli_multiple_backends_all_reachable() {
    let backend1 = TestBroker::start_with_config(backend_config()).await;
    let backend2 = TestBroker::start_with_config(backend_config()).await;

    let lb = TestBroker::start_with_config(lb_config(vec![
        backend1.address().to_string(),
        backend2.address().to_string(),
    ]))
    .await;

    for i in 0..4 {
        let result = run_cli_command(&[
            "pub",
            "--url",
            lb.address(),
            "--topic",
            &format!("test/lb/multi/{i}"),
            "--message",
            &format!("msg-{i}"),
            "--client-id",
            &format!("lb-multi-{i}"),
            "--non-interactive",
        ])
        .await;
        assert!(
            result.success,
            "Client {i} should connect through LB: {}",
            result.stderr
        );
    }

    drop(lb);
    drop(backend1);
    drop(backend2);
}

#[tokio::test]
async fn test_cli_qos1_through_lb_redirect() {
    let backend = TestBroker::start_with_config(backend_config()).await;
    let lb = TestBroker::start_with_config(lb_config(vec![backend.address().to_string()])).await;

    let sub_handle = run_cli_sub_async(backend.address(), "test/lb/qos1", 1, &["--qos", "1"]);
    tokio::time::sleep(Duration::from_millis(500)).await;

    let pub_result =
        run_cli_pub(lb.address(), "test/lb/qos1", "qos1-via-lb", &["--qos", "1"]).await;
    assert!(
        pub_result.success,
        "QoS 1 publish through LB failed: {}",
        pub_result.stderr
    );

    let sub_result = tokio::time::timeout(Duration::from_secs(3), sub_handle)
        .await
        .expect("Timeout waiting for subscriber")
        .expect("Subscriber task failed");

    assert!(
        sub_result.stdout_contains("qos1-via-lb"),
        "QoS 1 message should be delivered. stdout: {}, stderr: {}",
        sub_result.stdout,
        sub_result.stderr
    );

    drop(lb);
    drop(backend);
}
