#![cfg(feature = "broker")]

use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerShutdownHandle, MqttBroker};
use mqtt5::error::Result;
use mqtt5::{ConnectOptions, MqttClient};
use std::net::SocketAddr;
use std::path::Path;
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
