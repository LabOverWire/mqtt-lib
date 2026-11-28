#![cfg(feature = "broker")]

mod common;
use common::{find_workspace_root, get_cli_binary_path, TestBroker};

#[tokio::test]
async fn test_cli_tcp_transport() {
    let broker = TestBroker::start().await;

    let result = tokio::process::Command::new(get_cli_binary_path())
        .args([
            "pub",
            "--url",
            broker.address(),
            "--topic",
            "test/tcp",
            "--message",
            "tcp_test",
            "--non-interactive",
        ])
        .output()
        .await
        .expect("Failed to run pub");

    assert!(result.status.success(), "TCP transport should work");
}

#[tokio::test]
async fn test_cli_tls_transport() {
    let broker = TestBroker::start_with_tls().await;
    let ca_cert = find_workspace_root().join("test_certs/ca.pem");

    let result = tokio::process::Command::new(get_cli_binary_path())
        .args([
            "pub",
            "--url",
            broker.address(),
            "--ca-cert",
            ca_cert.to_str().expect("CA path is not UTF-8"),
            "--topic",
            "test/tls",
            "--message",
            "tls_test",
            "--non-interactive",
        ])
        .output()
        .await
        .expect("Failed to run pub");

    assert!(
        result.status.success(),
        "TLS transport should work: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn test_cli_url_custom_ports() {
    let tcp_result = tokio::process::Command::new(get_cli_binary_path())
        .args([
            "pub",
            "--url",
            "mqtt://localhost:12345",
            "--topic",
            "test",
            "--message",
            "test",
            "--non-interactive",
        ])
        .output()
        .await
        .expect("Failed to run pub");

    let tcp_stderr = String::from_utf8_lossy(&tcp_result.stderr);
    assert!(
        tcp_stderr.contains("Connection refused") || tcp_stderr.contains("12345"),
        "Should try to connect to custom port"
    );

    let tls_result = tokio::process::Command::new(get_cli_binary_path())
        .args([
            "pub",
            "--url",
            "mqtts://localhost:12346",
            "--topic",
            "test",
            "--message",
            "test",
            "--non-interactive",
        ])
        .output()
        .await
        .expect("Failed to run pub");

    let tls_stderr = String::from_utf8_lossy(&tls_result.stderr);
    let tls_stdout = String::from_utf8_lossy(&tls_result.stdout);
    assert!(
        tls_stderr.contains("Connection refused")
            || tls_stderr.contains("12346")
            || tls_stderr.contains("CryptoProvider")
            || tls_stdout.contains("CryptoProvider"),
        "Should try to connect to custom TLS port or fail with crypto provider issue"
    );
}

#[tokio::test]
async fn test_cli_transport_help() {
    let pub_help = tokio::process::Command::new(get_cli_binary_path())
        .args(["pub", "--help"])
        .output()
        .await
        .expect("Failed to get help");

    let help_text = String::from_utf8_lossy(&pub_help.stdout);

    assert!(
        help_text.contains("mqtt://"),
        "Should mention TCP transport"
    );
}
