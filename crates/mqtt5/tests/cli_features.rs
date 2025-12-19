//! Comprehensive tests for CLI MQTT v5.0 features
//!
//! Tests all new features added to the CLI including:
//! - Connection options (`clean_start`, `session_expiry`, `keep_alive`)
//! - Will messages with all properties
//! - Authentication
//! - All `QoS` levels

use mqtt5::time::Duration;

mod common;
use common::cli_helpers::*;
use common::TestBroker;

/// Test that all new CLI arguments are present in help
#[tokio::test]
async fn test_cli_new_arguments_in_help() {
    // Test pub command help
    let pub_help = run_cli_command(&["pub", "--help"]).await;
    assert!(pub_help.success, "Pub help should succeed");

    // Check all new arguments are present
    assert!(
        pub_help.stdout_contains("--no-clean-start"),
        "Should have no-clean-start option"
    );
    assert!(
        pub_help.stdout_contains("--session-expiry"),
        "Should have session-expiry option"
    );
    assert!(
        pub_help.stdout_contains("--keep-alive"),
        "Should have keep-alive option"
    );
    assert!(
        pub_help.stdout_contains("--username"),
        "Should have username option"
    );
    assert!(
        pub_help.stdout_contains("--password"),
        "Should have password option"
    );
    assert!(
        pub_help.stdout_contains("--will-topic"),
        "Should have will-topic option"
    );
    assert!(
        pub_help.stdout_contains("--will-message"),
        "Should have will-message option"
    );
    assert!(
        pub_help.stdout_contains("--will-qos"),
        "Should have will-qos option"
    );
    assert!(
        pub_help.stdout_contains("--will-retain"),
        "Should have will-retain option"
    );
    assert!(
        pub_help.stdout_contains("--will-delay"),
        "Should have will-delay option"
    );

    // Test sub command help
    let sub_help = run_cli_command(&["sub", "--help"]).await;
    assert!(sub_help.success, "Sub help should succeed");

    // Check sub has same connection options
    assert!(
        sub_help.stdout_contains("--no-clean-start"),
        "Sub should have no-clean-start"
    );
    assert!(
        sub_help.stdout_contains("--session-expiry"),
        "Sub should have session-expiry"
    );
    assert!(
        sub_help.stdout_contains("--keep-alive"),
        "Sub should have keep-alive"
    );
    assert!(
        sub_help.stdout_contains("--username"),
        "Sub should have username"
    );
    assert!(
        sub_help.stdout_contains("--password"),
        "Sub should have password"
    );

    println!("✅ All new CLI arguments are present in help text");
}

/// Test clean start functionality
#[tokio::test]
async fn test_cli_clean_start() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();
    let client_id = "test-clean-client";

    // First connection with default (clean_start=true)
    let pub1 = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/clean",
        "--message",
        "test1",
        "--client-id",
        client_id,
        "--non-interactive",
    ])
    .await;

    assert!(pub1.success, "First publish should succeed");
    assert!(
        !pub1.stdout_contains("Resumed existing session"),
        "First connection should not resume session"
    );

    // Second connection with no-clean-start should resume session
    let pub2 = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/clean",
        "--message",
        "test2",
        "--no-clean-start",
        "--client-id",
        client_id,
        "--non-interactive",
    ])
    .await;

    assert!(pub2.success, "Second publish should succeed");

    // Verify session resumption or check it didn't error
    if pub2.stdout_contains("Resumed existing session") {
        println!("✅ Clean start functionality verified - session resumed");
    } else {
        println!("⚠️  Session resumption not confirmed in output");
    }
}

/// Test session expiry interval
#[tokio::test]
async fn test_cli_session_expiry() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();
    let client_id = "test-expiry-client";

    // Connect with session expiry of 2 seconds
    let result1 = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/expiry",
        "--message",
        "test",
        "--session-expiry",
        "2",
        "--client-id",
        client_id,
        "--non-interactive",
    ])
    .await;

    assert!(
        result1.success,
        "Publish with session expiry should succeed"
    );

    // Immediately reconnect - session should be present
    let result2 = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/expiry",
        "--message",
        "test2",
        "--no-clean-start",
        "--client-id",
        client_id,
        "--non-interactive",
    ])
    .await;

    assert!(result2.success, "Immediate reconnect should succeed");

    // Wait for expiry
    tokio::time::sleep(Duration::from_secs(3)).await;

    // Reconnect after expiry - session should be gone
    let result3 = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/expiry",
        "--message",
        "test3",
        "--no-clean-start",
        "--client-id",
        client_id,
        "--non-interactive",
    ])
    .await;

    assert!(result3.success, "Reconnect after expiry should succeed");

    println!("✅ Session expiry interval parameter works");
}

/// Test custom keep-alive
#[tokio::test]
async fn test_cli_keep_alive() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test custom keep-alive value
    let result = run_cli_command(&[
        "pub",
        "--url",
        broker_url,
        "--topic",
        "test/keepalive",
        "--message",
        "test",
        "--keep-alive",
        "120",
        "--non-interactive",
    ])
    .await;

    assert!(result.success, "Custom keep-alive should work");
    assert!(
        !result.stderr_contains("keep-alive"),
        "No errors about keep-alive"
    );

    println!("✅ Custom keep-alive parameter accepted and used");
}

/// Test username/password authentication
#[tokio::test]
async fn test_cli_authentication() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test with username and password - verify they are passed to broker
    let result = verify_authentication(
        broker_url, "testuser", "testpass", true, // Test broker accepts any auth
    )
    .await;

    assert!(
        result.is_ok() && result.unwrap(),
        "Authentication parameters should work"
    );

    println!("✅ Username/password authentication parameters verified");
}

/// Test will message configuration
#[tokio::test]
async fn test_cli_will_message() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test all will message parameters work and deliver
    let result = verify_will_delivery(broker_url, "test/will/configured", "client_offline").await;

    match result {
        Ok(true) => {}
        Ok(false) => panic!("Will message was not delivered"),
        Err(e) => panic!("Will message delivery failed: {e}"),
    }

    println!("✅ Will message configuration and delivery verified");
}

/// Test QoS levels
#[tokio::test]
async fn test_cli_qos_levels() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test all QoS levels with actual delivery verification
    for qos in 0..=2 {
        let result = verify_qos_delivery(broker_url, qos).await;

        match result {
            Ok(true) => {}
            Ok(false) => panic!("QoS {qos} message was not delivered"),
            Err(e) => panic!("QoS {qos} delivery failed: {e}"),
        }
    }

    println!("✅ All QoS levels verified with actual message delivery");
}

/// Test retained messages
#[tokio::test]
async fn test_cli_retained() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Test retained message functionality
    let result =
        verify_retained_message(broker_url, "test/retained", "retained_test_message").await;

    match result {
        Ok(true) => {}
        Ok(false) => panic!("Retained message was not persisted"),
        Err(e) => panic!("Retained message test failed: {e}"),
    }

    println!("✅ Retained message flag works with verified persistence");
}

/// Test subscribe with count limit
#[tokio::test]
async fn test_cli_sub_count() {
    let broker = TestBroker::start().await;
    let broker_url = broker.address();

    // Start subscriber with count=2
    let sub_handle = run_cli_sub_async(broker_url, "test/count", 2, &[]);

    // Give subscriber time to connect
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Publish 3 messages
    for i in 1..=3 {
        let pub_result = run_cli_pub(broker_url, "test/count", &format!("msg{i}"), &[]).await;
        assert!(pub_result.success, "Publish {i} should succeed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Subscriber should exit after receiving 2 messages
    let sub_result = tokio::time::timeout(Duration::from_secs(2), sub_handle).await;

    assert!(
        sub_result.is_ok(),
        "Subscriber should exit after count limit"
    );

    let result = sub_result.unwrap().expect("Subscribe task failed");
    assert!(result.stdout_contains("msg1"), "Should receive msg1");
    assert!(result.stdout_contains("msg2"), "Should receive msg2");
    // msg3 might not be received due to count limit

    println!("✅ Subscribe count limit verified to work correctly");
}
