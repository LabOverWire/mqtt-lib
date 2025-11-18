//! Test for CLI TLS broker functionality

use mqtt5::time::Duration;
use tokio::process::Command;

#[tokio::test]
async fn test_cli_broker_tls_starts() {
    // Ensure CLI is built
    if !std::path::Path::new("target/release/mqttv5").exists() {
        Command::new("cargo")
            .args(["build", "--release", "-p", "mqttv5-cli"])
            .output()
            .await
            .expect("Failed to build CLI");
    }

    // Start broker with TLS
    let mut broker = Command::new("target/release/mqttv5")
        .args([
            "broker",
            "--tls-cert",
            "test_certs/server.pem",
            "--tls-key",
            "test_certs/server.key",
            "--non-interactive",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to start broker");

    // Give it time to start
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Check if process is still running (didn't crash)
    let status = broker.try_wait().expect("Failed to check broker status");

    if let Some(exit_status) = status {
        // Process exited, get output for debugging
        let output = broker
            .wait_with_output()
            .await
            .expect("Failed to get output");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        // Check if it's the expected crypto provider error (before our fix)
        if stderr.contains("CryptoProvider") || stdout.contains("CryptoProvider") {
            panic!("Broker crashed with CryptoProvider error - rustls not initialized properly");
        } else {
            panic!(
                "Broker exited unexpectedly. Exit status: {exit_status:?}\nStderr: {stderr}\nStdout: {stdout}"
            );
        }
    }

    // Broker is running, kill it
    broker.kill().await.expect("Failed to kill broker");

    println!("✅ TLS broker starts successfully without CryptoProvider error");
}

#[tokio::test]
#[ignore = "This test can be flaky in CI due to port binding"]
async fn test_cli_broker_tls_listens() {
    // Clean up any existing brokers
    Command::new("pkill")
        .args(["-f", "mqttv5"])
        .output()
        .await
        .ok();

    // Give time for cleanup
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Start broker with TLS in background
    let mut broker = Command::new("target/release/mqttv5")
        .args([
            "broker",
            "--tls-cert",
            "test_certs/server.pem",
            "--tls-key",
            "test_certs/server.key",
            "--tls-host",
            "127.0.0.1:28883", // Use different port to avoid conflicts
            "--verbose",
            "--non-interactive",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to start broker");

    // Give broker time to start and bind to ports
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Try to connect to the TLS port to verify it's listening
    let connection_result = tokio::net::TcpStream::connect("127.0.0.1:28883").await;

    // Kill broker
    broker.kill().await.ok();

    // Wait for cleanup
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Verify we could connect to the TLS port
    assert!(
        connection_result.is_ok(),
        "Could not connect to TLS port - broker may not be listening on TLS"
    );

    println!("✅ TLS broker listens on configured TLS port");
}
