#![cfg(feature = "broker")]

use mqtt5::broker::bridge::{BridgeConfig, BridgeDirection};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::storage::FileBackend;
use mqtt5::broker::{BrokerShutdownHandle, MqttBroker};
use mqtt5::error::Result;
use mqtt5::{ConnectOptions, MqttClient, QoS, SubscribeOptions};
use std::net::SocketAddr;
use std::path::Path;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

struct RunningBroker {
    addr: SocketAddr,
    shutdown: BrokerShutdownHandle,
    run: JoinHandle<Result<()>>,
}

impl RunningBroker {
    async fn stop(self) {
        self.shutdown.shutdown();
        self.run
            .await
            .expect("broker task joins")
            .expect("broker run returns cleanly");
    }
}

async fn start_on(dir: &Path) -> Result<RunningBroker> {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"))
        .with_storage(
            StorageConfig::new()
                .with_backend(StorageBackend::File)
                .with_base_dir(dir.to_path_buf()),
        );
    let mut broker = MqttBroker::with_config(config).await?;
    let addr = broker.local_addr().expect("broker address");
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    let run = tokio::spawn(async move { broker.run().await });
    ready.wait_for(|&up| up).await.expect("broker ready");
    Ok(RunningBroker {
        addr,
        shutdown,
        run,
    })
}

async fn connect_persistent(addr: SocketAddr, client_id: &str) -> bool {
    let client = MqttClient::new(client_id);
    let options = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_resume_existing_session(true);
    let connected = Box::pin(client.connect_with_options(&format!("mqtt://{addr}"), options))
        .await
        .expect("client connects");
    client.disconnect().await.expect("client disconnects");
    connected.session_present
}

#[tokio::test]
async fn second_broker_on_a_storage_directory_in_use_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = start_on(dir.path()).await.expect("first broker starts");

    let second = start_on(dir.path()).await;
    let Err(error) = second else {
        panic!("second broker started on a storage directory in use");
    };
    let message = error.to_string();
    assert!(message.contains("already in use"), "{message}");
    assert!(
        message.contains(&dir.path().display().to_string()),
        "{message}"
    );

    first.stop().await;
}

#[tokio::test]
async fn storage_directory_is_released_when_the_broker_shuts_down() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = start_on(dir.path()).await.expect("first broker starts");
    assert!(!connect_persistent(first.addr, "lock-client-a").await);
    assert!(!connect_persistent(first.addr, "lock-client-b").await);
    first.stop().await;

    let restarted = start_on(dir.path())
        .await
        .expect("broker starts once the directory is released");
    assert!(connect_persistent(restarted.addr, "lock-client-a").await);
    assert!(connect_persistent(restarted.addr, "lock-client-b").await);
    restarted.stop().await;
}

async fn stay_connected(addr: SocketAddr, client_id: &str) -> MqttClient {
    let client = MqttClient::new(client_id);
    let options = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_automatic_reconnect(false);
    Box::pin(client.connect_with_options(&format!("mqtt://{addr}"), options))
        .await
        .expect("client connects");
    client
        .subscribe_with_options(
            &format!("held/{client_id}"),
            SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            |_| {},
        )
        .await
        .expect("client subscribes");
    client
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_returns_after_connected_clients_have_released_the_directory() {
    let dir = tempfile::tempdir().expect("temp dir");
    let first = start_on(dir.path()).await.expect("first broker starts");
    let mut clients = Vec::new();
    for i in 0..20 {
        clients.push(stay_connected(first.addr, &format!("held-client-{i}")).await);
    }
    first.stop().await;

    let restarted = start_on(dir.path())
        .await
        .expect("directory is free as soon as run returns");
    drop(clients);
    for i in 0..20 {
        assert!(connect_persistent(restarted.addr, &format!("held-client-{i}")).await);
    }
    restarted.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bridged_broker_releases_the_directory_promptly_after_shutdown() {
    let mut remote = MqttBroker::with_config(
        BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"))
            .with_storage(StorageConfig::new().with_backend(StorageBackend::Memory)),
    )
    .await
    .expect("remote broker starts");
    let remote_addr = remote.local_addr().expect("remote address");
    let mut remote_ready = remote.ready_receiver();
    let remote_shutdown = remote.shutdown_handle();
    let remote_run = tokio::spawn(async move { remote.run().await });
    remote_ready.wait_for(|&up| up).await.expect("remote ready");

    let dir = tempfile::tempdir().expect("temp dir");
    let mut bridge = BridgeConfig::new("lock-bridge", remote_addr.to_string()).add_topic(
        "bridged/#",
        BridgeDirection::Both,
        QoS::AtLeastOnce,
    );
    bridge.initial_reconnect_delay = Duration::from_millis(200);
    let mut config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"))
        .with_storage(
            StorageConfig::new()
                .with_backend(StorageBackend::File)
                .with_base_dir(dir.path().to_path_buf()),
        );
    config.bridges = vec![bridge];
    let mut local = MqttBroker::with_config(config)
        .await
        .expect("bridged broker starts");
    let mut ready = local.ready_receiver();
    let shutdown = local.shutdown_handle();
    let run = tokio::spawn(async move { local.run().await });
    ready
        .wait_for(|&up| up)
        .await
        .expect("bridged broker ready");
    tokio::time::sleep(Duration::from_millis(100)).await;

    shutdown.shutdown();
    run.await
        .expect("broker task joins")
        .expect("broker run returns cleanly");
    let began = Instant::now();
    let mut attempt_began = began;
    let mut reopened = FileBackend::new(dir.path()).await;
    while reopened.is_err() && began.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(5)).await;
        attempt_began = Instant::now();
        reopened = FileBackend::new(dir.path()).await;
    }
    let held = attempt_began.duration_since(began);
    drop(reopened.expect("directory is released after shutdown"));
    assert!(
        held < Duration::from_millis(400),
        "directory held for {held:?} after run returned"
    );

    remote_shutdown.shutdown();
    remote_run
        .await
        .expect("remote task joins")
        .expect("remote run returns cleanly");
}
