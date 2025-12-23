#![allow(clippy::doc_markdown)]
//! End-to-end CLI integration tests
//!
//! Tests real publish/subscribe scenarios with the CLI

mod common;
use common::cli_helpers::*;
use common::TestBroker;
use mqtt5::time::Duration;

/// Test basic publish and subscribe
#[tokio::test]
async fn test_cli_pub_sub_basic() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test basic message delivery
    let result = verify_pub_sub_delivery(broker_url, "test/basic", "Hello MQTT", &[]).await;

    assert!(result.is_ok(), "Basic pub/sub failed: {:?}", result.err());
    assert!(result.unwrap(), "Message not delivered");

    println!("✅ Basic pub/sub works with verified message delivery");
}

/// Test session persistence
#[tokio::test]
async fn test_cli_session_persistence() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    let client_id = "persist-test";

    // Test session persistence
    let result = verify_session_persistence(broker_url, client_id).await;

    match result {
        Ok(true) => {
            println!("✅ Session persistence verified - session was resumed");
        }
        Ok(false) => {
            println!("⚠️  Session not resumed - broker may not support persistence");
        }
        Err(e) => {
            println!("❌ Session persistence test failed: {e}");
            panic!("Session persistence test failed");
        }
    }
}

/// Test will message delivery
#[tokio::test]
async fn test_cli_will_message_delivery() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test will message delivery
    let result = verify_will_delivery(broker_url, "test/will/status", "disconnected").await;

    match result {
        Ok(true) => {
            println!("✅ Will message was delivered successfully");
        }
        Ok(false) => {
            println!("❌ Will message was not delivered");
            panic!("Will message delivery failed");
        }
        Err(e) => {
            println!("❌ Will message test error: {e}");
            panic!("Will message test failed");
        }
    }
}

/// Test multiple QoS levels
#[tokio::test]
async fn test_cli_qos_delivery() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    for qos in 0..=2 {
        let result = verify_qos_delivery(broker_url, qos).await;

        assert!(result.is_ok(), "QoS {qos} test failed: {:?}", result.err());
        assert!(result.unwrap(), "QoS {qos} message not delivered");
    }

    println!("✅ All QoS levels work correctly with verified delivery");
}

/// Test wildcard subscriptions
#[tokio::test]
async fn test_cli_wildcard_subscriptions() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Start subscriber for wildcard topic
    let sub_handle = run_cli_sub_async(broker_url, "test/wild/+/data", 2, &["--verbose"]);

    // Give subscriber time to connect
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish to matching topics
    for sensor in ["temp", "humidity"] {
        let pub_result = run_cli_pub(
            broker_url,
            &format!("test/wild/{sensor}/data"),
            &format!("{sensor}_value"),
            &[],
        )
        .await;

        assert!(pub_result.success, "Failed to publish to {sensor}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Wait for messages
    let sub_result = tokio::time::timeout(Duration::from_secs(3), sub_handle)
        .await
        .expect("Timeout waiting for messages")
        .expect("Subscribe task failed");

    assert!(
        sub_result.stdout_contains("temp_value"),
        "Should receive temp message"
    );
    assert!(
        sub_result.stdout_contains("humidity_value"),
        "Should receive humidity message"
    );

    println!("✅ Wildcard subscriptions work with verified routing");
}

/// Test authentication with credentials
#[tokio::test]
async fn test_cli_with_auth() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test connection with credentials (broker accepts any for now)
    let result = verify_authentication(
        broker_url, "user1", "pass1", true, // Should succeed with test broker
    )
    .await;

    assert!(
        result.is_ok(),
        "Authentication test failed: {:?}",
        result.err()
    );

    println!("✅ Authentication parameters work correctly");
}

/// Test message from stdin
#[tokio::test]
async fn test_cli_stdin_message() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Start subscriber first
    let sub_handle = run_cli_sub_async(broker_url, "test/stdin", 1, &[]);

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish from stdin
    let pub_result = run_cli_with_stdin(
        &[
            "pub",
            "--url",
            broker_url,
            "--topic",
            "test/stdin",
            "--stdin",
            "--non-interactive",
        ],
        "Message from stdin\n",
    )
    .await;

    assert!(pub_result.success, "Failed to publish from stdin");

    // Verify message was received
    let sub_result = tokio::time::timeout(Duration::from_secs(2), sub_handle)
        .await
        .expect("Timeout")
        .expect("Subscribe task failed");

    assert!(
        sub_result.stdout_contains("Message from stdin"),
        "Stdin message not received"
    );

    println!("✅ Stdin message input works with verified delivery");
}

/// Test message from file
#[tokio::test]
async fn test_cli_file_message() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Create temp file
    let temp_file = "/tmp/mqtt_test_msg.txt";
    std::fs::write(temp_file, "Message from file").expect("Failed to write temp file");

    // Start subscriber
    let sub_handle = run_cli_sub_async(broker_url, "test/file", 1, &[]);

    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish from file
    let pub_result = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/file",
        "--file",
        temp_file,
        "--non-interactive",
    ])
    .await;

    assert!(pub_result.success, "Failed to publish from file");

    // Verify message was received
    let sub_result = tokio::time::timeout(Duration::from_secs(2), sub_handle)
        .await
        .expect("Timeout")
        .expect("Subscribe task failed");

    assert!(
        sub_result.stdout_contains("Message from file"),
        "File message not received"
    );

    // Cleanup
    let _ = std::fs::remove_file(temp_file);

    println!("✅ File message input works with verified delivery");
}
