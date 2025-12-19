//! CLI Functionality Integration Tests
//!
//! Tests that validate our mqttv5 CLI tool works correctly in real scenarios.
//! These tests demonstrate that we can replace mosquitto tools completely.

use mqtt5::time::Duration;

mod common;
use common::cli_helpers::*;
use common::TestBroker;

/// Test broker functionality with actual broker startup
#[tokio::test]
async fn test_cli_broker_functionality() {
    // Test broker help
    let help_result = run_cli_command(&["broker", "--help"]).await;
    assert!(help_result.success, "Broker help should succeed");
    assert!(
        help_result.stdout_contains("--host"),
        "Should show host option"
    );

    // Note: We use TestBroker from library instead of CLI broker for tests
    // This ensures tests are reliable and don't conflict with system ports
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Verify we can connect to the test broker
    let result = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/broker",
        "--message",
        "broker_test",
        "--non-interactive",
    ])
    .await;

    assert!(result.success, "Should connect to test broker");
    println!("✅ CLI broker functionality verified");
}

/// Test CLI pub/sub with actual broker communication
#[tokio::test]
async fn test_cli_pub_sub_functionality() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test simple publish
    let pub_result = run_cli_pub(broker_url, "test/simple", "Hello from CLI", &[]).await;

    assert!(pub_result.success, "Publish should succeed");

    // Test subscribe and receive
    let verify_result =
        verify_pub_sub_delivery(broker_url, "test/delivery", "Message to verify", &[]).await;

    assert!(
        verify_result.is_ok() && verify_result.unwrap(),
        "Pub/sub delivery should work"
    );

    // Test with verbose flag
    let sub_handle = run_cli_sub_async(broker_url, "test/verbose", 1, &["--verbose"]);

    tokio::time::sleep(Duration::from_millis(500)).await;

    let _ = run_cli_pub(broker_url, "test/verbose", "Verbose test", &[]).await;

    let sub_result = tokio::time::timeout(Duration::from_secs(2), sub_handle)
        .await
        .expect("Timeout")
        .expect("Subscribe failed");

    // With verbose, should show topic name
    assert!(
        sub_result.stdout_contains("test/verbose"),
        "Verbose mode should show topic"
    );

    println!("✅ CLI pub/sub functionality verified with real broker");
}

/// Test CLI input validation and error messages
#[tokio::test]
async fn test_cli_validation() {
    // Test invalid topic validation
    let result = run_cli_command(&[
        "pub",
        "--topic",
        "test//invalid",
        "--message",
        "test",
        "--non-interactive",
    ])
    .await;

    assert!(!result.success, "Should fail with invalid topic");
    assert!(
        result.stderr_contains("cannot have empty segments"),
        "Should show helpful error message"
    );
    assert!(
        result.stderr_contains("Did you mean 'test/invalid'?"),
        "Should suggest correction"
    );

    // Test invalid QoS value
    let qos_result = run_cli_command(&[
        "pub",
        "--topic",
        "test/qos",
        "--message",
        "test",
        "--qos",
        "3",
        "--non-interactive",
    ])
    .await;

    assert!(!qos_result.success, "Should fail with invalid QoS");
    assert!(
        qos_result.stderr_contains("QoS must be 0, 1, or 2"),
        "Should show QoS error"
    );

    println!("✅ CLI validation tests successful");
}

/// Test CLI with multiple topics and patterns
#[tokio::test]
async fn test_cli_topic_patterns() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test multilevel wildcard
    let sub_handle = run_cli_sub_async(broker_url, "sensors/#", 3, &["--verbose"]);

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish to various matching topics
    for topic in [
        "sensors/temp",
        "sensors/room1/humidity",
        "sensors/outdoor/pressure",
    ] {
        let _ = run_cli_pub(broker_url, topic, &format!("data_{topic}"), &[]).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let sub_result = tokio::time::timeout(Duration::from_secs(3), sub_handle)
        .await
        .expect("Timeout")
        .expect("Subscribe failed");

    // Should receive all three messages
    assert!(sub_result.stdout_contains("data_sensors/temp"));
    assert!(sub_result.stdout_contains("data_sensors/room1/humidity"));
    assert!(sub_result.stdout_contains("data_sensors/outdoor/pressure"));

    println!("✅ CLI topic pattern handling verified");
}

/// Test CLI performance with actual message throughput
#[tokio::test]
async fn test_cli_message_throughput() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Start subscriber for throughput test
    let sub_handle = run_cli_sub_async(broker_url, "perf/test", 10, &[]);

    tokio::time::sleep(Duration::from_millis(500)).await;

    let start = std::time::Instant::now();

    // Publish 10 messages rapidly
    for i in 0..10 {
        let pub_result = run_cli_pub(broker_url, "perf/test", &format!("perf_msg_{i}"), &[]).await;
        assert!(pub_result.success, "Publish {i} should succeed");
    }

    let publish_duration = start.elapsed();

    // Wait for all messages to be received
    let sub_result = tokio::time::timeout(Duration::from_secs(5), sub_handle)
        .await
        .expect("Timeout")
        .expect("Subscribe failed");

    // Verify all messages received
    for i in 0..10 {
        assert!(
            sub_result.stdout_contains(&format!("perf_msg_{i}")),
            "Should receive message {i}"
        );
    }

    let total_duration = start.elapsed();
    let msgs_per_sec = 10.0 / total_duration.as_secs_f64();

    println!("✅ CLI performance test:");
    println!("   Published 10 messages in: {publish_duration:?}");
    println!("   Total round-trip time: {total_duration:?}");
    println!("   Throughput: {msgs_per_sec:.1} messages/second");
}

/// Test CLI with both long and short flags
#[tokio::test]
async fn test_cli_flag_formats() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test long flags
    let long_result = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/long",
        "--message",
        "long flags",
        "--qos",
        "1",
        "--non-interactive",
    ])
    .await;

    assert!(long_result.success, "Long flags should work");

    // Test short flags
    let short_result = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "-t",
        "test/short",
        "-m",
        "short flags",
        "-q",
        "1",
        "--non-interactive",
    ])
    .await;

    assert!(short_result.success, "Short flags should work");

    // Test mixed flags
    let mixed_result = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "-t",
        "test/mixed",
        "--message",
        "mixed flags",
        "-q",
        "2",
        "--non-interactive",
    ])
    .await;

    assert!(mixed_result.success, "Mixed flags should work");

    println!("✅ CLI ergonomics verified - all flag formats work");
}
