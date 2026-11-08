#![cfg(all(feature = "broker", feature = "transport-websocket"))]

use mqtt5::broker::config::{
    BrokerConfig, LoadBalancerConfig, StorageBackend, StorageConfig, TlsConfig as BrokerTlsConfig,
    WebSocketConfig as BrokerWebSocketConfig,
};
use mqtt5::broker::MqttBroker;
use mqtt5::transport::tls::TlsConfig;
use mqtt5::transport::websocket::WebSocketConfig;
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::TcpListener;

fn certs() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs")
}

fn trusted_ca() -> Vec<u8> {
    std::fs::read(certs().join("ca.pem")).unwrap()
}

fn base() -> BrokerConfig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        })
}

fn broker_tls_files(cert: PathBuf, key: PathBuf) -> BrokerTlsConfig {
    BrokerTlsConfig::new(cert, key)
        .with_require_client_cert(false)
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
}

fn broker_tls() -> BrokerTlsConfig {
    broker_tls_files(certs().join("server.pem"), certs().join("server.key"))
}

async fn run(mut broker: MqttBroker) -> tokio::task::JoinHandle<()> {
    let mut ready = broker.ready_receiver();
    let handle = tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&v| v).await.unwrap();
    handle
}

struct OtherCa {
    cert: PathBuf,
    key: PathBuf,
    pem: Vec<u8>,
    dir: tempfile::TempDir,
}

fn signed_by_another_ca(names: &[&str]) -> OtherCa {
    use rcgen::{BasicConstraints, CertificateParams, IsCa, Issuer, KeyPair};
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca_params, ca_key);
    let leaf_key = KeyPair::generate().unwrap();
    let leaf = CertificateParams::new(names.iter().map(ToString::to_string).collect::<Vec<_>>())
        .unwrap()
        .signed_by(&leaf_key, &issuer)
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    std::fs::write(&cert, leaf.pem()).unwrap();
    std::fs::write(&key, leaf_key.serialize_pem()).unwrap();
    OtherCa {
        cert,
        key,
        pem: ca.pem().into_bytes(),
        dir,
    }
}

async fn wss_broker(
    tls: BrokerTlsConfig,
    balances_to: Option<String>,
) -> (u16, tokio::task::JoinHandle<()>) {
    let port = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut config = base().with_tls(tls).with_websocket_tls(
        BrokerWebSocketConfig::new()
            .with_bind_addresses(vec![format!("127.0.0.1:{port}").parse().unwrap()])
            .with_path("/mqtt".to_string()),
    );
    if let Some(reference) = balances_to {
        config = config.with_load_balancer(LoadBalancerConfig::new(vec![reference]));
    }
    let broker = MqttBroker::with_config(config).await.unwrap();
    (port, run(broker).await)
}

fn no_verification() -> WebSocketConfig {
    WebSocketConfig::new("wss://localhost:1/mqtt")
        .unwrap()
        .with_tls_config(
            TlsConfig::new("127.0.0.1:0".parse().unwrap(), "localhost")
                .with_verify_server_cert(false),
        )
}

async fn connect_with_unverifying_websocket_config(url: &str, id: &str) -> Result<(), String> {
    let client = MqttClient::new(id);
    client.set_tls_config(None, None, Some(trusted_ca())).await;
    tokio::time::timeout(
        Duration::from_secs(5),
        client.connect_with_options(
            url,
            ConnectOptions::new(id)
                .with_automatic_reconnect(false)
                .with_websocket_config(no_verification()),
        ),
    )
    .await
    .map_err(|e| e.to_string())?
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_redirect_from_mqtts_to_wss_does_not_take_the_websocket_configurations_tls() {
    let target = signed_by_another_ca(&["localhost"]);
    let (wss_port, wss) = wss_broker(
        broker_tls_files(target.cert.clone(), target.key.clone()),
        None,
    )
    .await;
    let balancer = MqttBroker::with_config(base().with_tls(broker_tls()).with_load_balancer(
        LoadBalancerConfig::new(vec![format!("wss://localhost:{wss_port}/mqtt")]),
    ))
    .await
    .unwrap();
    let lb_port = balancer.tls_local_addr().unwrap().port();
    let lb = run(balancer).await;
    let outcome = connect_with_unverifying_websocket_config(
        &format!("mqtts://localhost:{lb_port}"),
        "mqtts-to-wss",
    )
    .await;
    lb.abort();
    wss.abort();
    let error = outcome.expect_err("credentials would go to an unverified server");
    assert!(error.contains("UnknownIssuer"), "{error}");
}

#[tokio::test]
async fn a_second_hop_from_wss_does_not_take_the_websocket_configurations_tls_either() {
    let target = signed_by_another_ca(&["localhost"]);
    let (last_port, last) = wss_broker(
        broker_tls_files(target.cert.clone(), target.key.clone()),
        None,
    )
    .await;
    let (middle_port, middle) = wss_broker(
        broker_tls(),
        Some(format!("wss://localhost:{last_port}/mqtt")),
    )
    .await;
    let balancer = MqttBroker::with_config(base().with_tls(broker_tls()).with_load_balancer(
        LoadBalancerConfig::new(vec![format!("wss://localhost:{middle_port}/mqtt")]),
    ))
    .await
    .unwrap();
    let lb_port = balancer.tls_local_addr().unwrap().port();
    let lb = run(balancer).await;
    let outcome = connect_with_unverifying_websocket_config(
        &format!("mqtts://localhost:{lb_port}"),
        "mqtts-wss-wss",
    )
    .await;
    lb.abort();
    middle.abort();
    last.abort();
    let error = outcome.expect_err("credentials would go to an unverified server");
    assert!(error.contains("UnknownIssuer"), "{error}");
}

#[tokio::test]
async fn a_redirected_connect_with_tls_is_checked_for_the_targets_name() {
    let target = signed_by_another_ca(&["localhost"]);
    let target_broker = MqttBroker::with_config(
        base().with_tls(broker_tls_files(target.cert.clone(), target.key.clone())),
    )
    .await
    .unwrap();
    let target_port = target_broker.tls_local_addr().unwrap().port();
    let target_handle = run(target_broker).await;
    let outcome_for = |reference: String, pem: Vec<u8>| async move {
        let balancer = MqttBroker::with_config(
            base()
                .with_tls(broker_tls())
                .with_load_balancer(LoadBalancerConfig::new(vec![reference])),
        )
        .await
        .unwrap();
        let lb_port = balancer.tls_local_addr().unwrap().port();
        let lb = run(balancer).await;
        let mut tls = TlsConfig::new(format!("127.0.0.1:{lb_port}").parse().unwrap(), "localhost")
            .with_system_roots(false);
        let mut bundle = trusted_ca();
        bundle.extend_from_slice(b"\n");
        bundle.extend_from_slice(&pem);
        tls.load_ca_cert_pem_bytes(&bundle).unwrap();
        let client = MqttClient::new("name-check");
        let outcome = client
            .connect_with_tls_and_options(
                tls,
                ConnectOptions::new("name-check").with_automatic_reconnect(false),
            )
            .await;
        lb.abort();
        outcome.map(|_| ()).map_err(|e| e.to_string())
    };
    let other_name = outcome_for(format!("127.0.0.1:{target_port}"), target.pem.clone()).await;
    let its_name = outcome_for(format!("localhost:{target_port}"), target.pem.clone()).await;
    drop(target.dir);
    target_handle.abort();
    assert!(
        other_name.is_err(),
        "a certificate for another name was accepted"
    );
    assert!(its_name.is_ok(), "{its_name:?}");
}

#[tokio::test]
async fn a_connection_started_over_mqtts_is_not_kept_from_mqtts_by_a_wss_hop() {
    let target = MqttBroker::with_config(base().with_tls(broker_tls()))
        .await
        .unwrap();
    let target_port = target.tls_local_addr().unwrap().port();
    let target_handle = run(target).await;
    let (middle_port, middle) = wss_broker(
        broker_tls(),
        Some(format!("mqtts://localhost:{target_port}")),
    )
    .await;
    let balancer = MqttBroker::with_config(base().with_tls(broker_tls()).with_load_balancer(
        LoadBalancerConfig::new(vec![format!("wss://localhost:{middle_port}/mqtt")]),
    ))
    .await
    .unwrap();
    let lb_port = balancer.tls_local_addr().unwrap().port();
    let lb = run(balancer).await;
    let outcome = connect_with_unverifying_websocket_config(
        &format!("mqtts://localhost:{lb_port}"),
        "mqtts-wss-mqtts",
    )
    .await;
    lb.abort();
    middle.abort();
    target_handle.abort();
    assert!(outcome.is_ok(), "{outcome:?}");
}
