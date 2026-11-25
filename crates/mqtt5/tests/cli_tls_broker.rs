#![cfg(feature = "broker")]

mod common;
use common::{find_workspace_root, get_cli_binary_path};
use mqtt5::time::Duration;
use tokio::process::Command;

fn tls_broker_command(tls_host: &str) -> Command {
    let workspace_root = find_workspace_root();
    let mut command = Command::new(get_cli_binary_path());
    command
        .arg("broker")
        .arg("--tls-cert")
        .arg(workspace_root.join("test_certs/server.pem"))
        .arg("--tls-key")
        .arg(workspace_root.join("test_certs/server.key"))
        .args([
            "--host",
            "127.0.0.1:0",
            "--tls-host",
            tls_host,
            "--quic-host",
            "127.0.0.1:0",
            "--allow-anonymous",
            "--non-interactive",
            "--storage-backend",
            "memory",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command
}

#[tokio::test]
async fn test_cli_broker_tls_starts() {
    let mut broker = tls_broker_command("127.0.0.1:0")
        .spawn()
        .expect("Failed to start broker");

    tokio::time::sleep(Duration::from_secs(2)).await;

    let status = broker.try_wait().expect("Failed to check broker status");

    if let Some(exit_status) = status {
        let output = broker
            .wait_with_output()
            .await
            .expect("Failed to get output");
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stderr.contains("CryptoProvider") && !stdout.contains("CryptoProvider"),
            "Broker crashed with CryptoProvider error - rustls not initialized properly"
        );
        panic!(
            "Broker exited unexpectedly. Exit status: {exit_status:?}\nStderr: {stderr}\nStdout: {stdout}"
        );
    }

    broker.kill().await.expect("Failed to kill broker");
}

#[tokio::test]
#[ignore = "This test can be flaky in CI due to port binding"]
async fn test_cli_broker_tls_listens() {
    let mut broker = tls_broker_command("127.0.0.1:28883")
        .arg("--verbose")
        .spawn()
        .expect("Failed to start broker");

    tokio::time::sleep(Duration::from_secs(4)).await;

    let connection_result = tokio::net::TcpStream::connect("127.0.0.1:28883").await;

    broker.kill().await.ok();

    assert!(
        connection_result.is_ok(),
        "Could not connect to TLS port - broker may not be listening on TLS"
    );
}
