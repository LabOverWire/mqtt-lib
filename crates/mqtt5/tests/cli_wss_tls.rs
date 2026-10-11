#![cfg(feature = "broker")]
mod common;
use common::cli_helpers::{run_cli_command, run_cli_pub, CliResult};
use common::{find_workspace_root, TestBroker};
use mqtt5::broker::config::{
    BrokerConfig, StorageBackend, StorageConfig, TlsConfig as BrokerTlsConfig, WebSocketConfig,
};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};

static WSS_PORT_COUNTER: AtomicU16 = AtomicU16::new(21200);

async fn start_wss_broker() -> (TestBroker, String) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let workspace_root = find_workspace_root();
    let wss_port = WSS_PORT_COUNTER.fetch_add(1, Ordering::SeqCst);

    let tls_config = BrokerTlsConfig::new(
        workspace_root.join("test_certs/server.pem"),
        workspace_root.join("test_certs/server.key"),
    )
    .with_require_client_cert(false)
    .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap());

    let ws_tls_config = WebSocketConfig::new()
        .with_bind_addresses(vec![format!("127.0.0.1:{wss_port}").parse().unwrap()])
        .with_path("/mqtt".to_string());

    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config)
        .with_tls(tls_config)
        .with_websocket_tls(ws_tls_config);

    let broker = TestBroker::start_with_config(config).await;
    (broker, format!("wss://localhost:{wss_port}/mqtt"))
}

fn ca_cert() -> String {
    find_workspace_root()
        .join("test_certs/ca.pem")
        .display()
        .to_string()
}

fn assert_succeeded(command: &str, result: &CliResult) {
    assert!(
        result.success,
        "{command} over wss:// with --ca-cert failed\nstdout: {}\nstderr: {}",
        result.stdout, result.stderr
    );
}

#[tokio::test]
async fn pub_uses_ca_cert_for_wss() {
    let (_broker, url) = start_wss_broker().await;
    let ca = ca_cert();

    let result = run_cli_pub(&url, "cli/wss/pub", "hello", &["--ca-cert", &ca]).await;

    assert_succeeded("pub", &result);
}

#[tokio::test]
async fn sub_uses_ca_cert_for_wss() {
    let (_broker, url) = start_wss_broker().await;
    let ca = ca_cert();

    let published = run_cli_pub(
        &url,
        "cli/wss/sub",
        "retained",
        &["--ca-cert", &ca, "--retain"],
    )
    .await;
    assert_succeeded("pub", &published);

    let result = run_cli_command(&[
        "sub",
        "--url",
        &url,
        "--topic",
        "cli/wss/sub",
        "--count",
        "1",
        "--non-interactive",
        "--ca-cert",
        &ca,
    ])
    .await;

    assert_succeeded("sub", &result);
    assert!(
        result.stdout_contains("retained"),
        "stdout: {}",
        result.stdout
    );
}

#[tokio::test]
async fn bench_throughput_uses_ca_cert_for_wss() {
    let (_broker, url) = start_wss_broker().await;
    let ca = ca_cert();

    let result = run_cli_command(&[
        "bench",
        "--url",
        &url,
        "--duration",
        "1",
        "--warmup",
        "0",
        "--ca-cert",
        &ca,
    ])
    .await;

    assert_succeeded("bench --mode throughput", &result);
}

#[tokio::test]
async fn bench_connections_uses_ca_cert_for_wss() {
    let (_broker, url) = start_wss_broker().await;
    let ca = ca_cert();

    let result = run_cli_command(&[
        "bench",
        "--mode",
        "connections",
        "--url",
        &url,
        "--duration",
        "1",
        "--warmup",
        "0",
        "--concurrency",
        "1",
        "--ca-cert",
        &ca,
    ])
    .await;

    assert_succeeded("bench --mode connections", &result);
    let output: serde_json::Value = serde_json::from_str(&result.stdout).unwrap();
    let results = &output["results"];
    assert_eq!(results["failed"], 0, "stderr: {}", result.stderr);
    assert!(
        results["successful"].as_u64().unwrap() > 0,
        "stderr: {}",
        result.stderr
    );
}
