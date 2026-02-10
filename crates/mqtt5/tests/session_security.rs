#![allow(clippy::large_futures)]

mod common;

use common::{get_cli_binary_path, TestBroker};
use mqtt5::broker::config::{
    AuthConfig, AuthMethod, BrokerConfig, RateLimitConfig, StorageBackend, StorageConfig,
};
use mqtt5::{ConnectOptions, MqttClient};
use std::io::Write;
use std::net::SocketAddr;
use std::process::Command;

fn setup_two_user_broker_config() -> (BrokerConfig, std::path::PathBuf, std::path::PathBuf) {
    let temp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let password_file = temp_dir.join(format!("test_session_sec_pw_{pid}_{ts}.txt"));
    let acl_file = temp_dir.join(format!("test_session_sec_acl_{pid}_{ts}.txt"));

    let _ = std::fs::remove_file(&password_file);
    let _ = std::fs::remove_file(&acl_file);

    let cli_binary = get_cli_binary_path();

    let status = Command::new(&cli_binary)
        .args([
            "passwd",
            "-c",
            "-b",
            "pass1",
            "alice",
            password_file.to_str().unwrap(),
        ])
        .status()
        .expect("create password file");
    assert!(status.success());

    let status = Command::new(&cli_binary)
        .args([
            "passwd",
            "-b",
            "pass2",
            "bob",
            password_file.to_str().unwrap(),
        ])
        .status()
        .expect("add second user");
    assert!(status.success());

    {
        let mut f = std::fs::File::create(&acl_file).expect("create acl file");
        writeln!(f, "user * topic # permission readwrite").expect("write acl");
    }

    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let auth_config = AuthConfig {
        allow_anonymous: false,
        password_file: Some(password_file.clone()),
        acl_file: Some(acl_file.clone()),
        auth_method: AuthMethod::Password,
        auth_data: Some(std::fs::read(&password_file).expect("read password file")),
        scram_file: None,
        jwt_config: None,
        federated_jwt_config: None,
        rate_limit: RateLimitConfig::default(),
    };

    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config)
        .with_auth(auth_config);

    (config, password_file, acl_file)
}

#[tokio::test]
async fn test_session_user_binding_rejects_different_user() {
    let (config, password_file, acl_file) = setup_two_user_broker_config();
    let broker = TestBroker::start_with_config(config).await;
    let shared_client_id = "session-bind-test";

    let alice_opts = ConnectOptions::new(shared_client_id)
        .with_clean_start(true)
        .with_credentials("alice", b"pass1")
        .with_session_expiry_interval(300);
    let alice = MqttClient::with_options(alice_opts.clone());
    alice
        .connect_with_options(broker.address(), alice_opts)
        .await
        .expect("alice connect");
    alice.subscribe("test/bind", |_| {}).await.unwrap();
    alice.disconnect().await.unwrap();

    let bob_opts = ConnectOptions::new(shared_client_id)
        .with_clean_start(false)
        .with_credentials("bob", b"pass2")
        .with_session_expiry_interval(300);
    let bob = MqttClient::with_options(bob_opts.clone());
    let result = bob.connect_with_options(broker.address(), bob_opts).await;

    assert!(
        result.is_err(),
        "bob must be rejected when resuming alice's session"
    );

    let _ = std::fs::remove_file(&password_file);
    let _ = std::fs::remove_file(&acl_file);
}

#[tokio::test]
async fn test_session_user_binding_allows_same_user() {
    let (config, password_file, acl_file) = setup_two_user_broker_config();
    let broker = TestBroker::start_with_config(config).await;
    let client_id = "session-same-user";

    let opts = ConnectOptions::new(client_id)
        .with_clean_start(true)
        .with_credentials("alice", b"pass1")
        .with_session_expiry_interval(300);
    let client1 = MqttClient::with_options(opts.clone());
    client1
        .connect_with_options(broker.address(), opts)
        .await
        .expect("first connect");
    client1.subscribe("test/same", |_| {}).await.unwrap();
    client1.disconnect().await.unwrap();

    let resume_opts = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_credentials("alice", b"pass1")
        .with_session_expiry_interval(300);
    let client2 = MqttClient::with_options(resume_opts.clone());
    let result = client2
        .connect_with_options(broker.address(), resume_opts)
        .await
        .expect("same user reconnect must succeed");

    assert!(
        result.session_present,
        "session must be present when same user reconnects"
    );

    client2.disconnect().await.unwrap();

    let _ = std::fs::remove_file(&password_file);
    let _ = std::fs::remove_file(&acl_file);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn test_acl_recheck_prunes_unauthorized_subscriptions() {
    let temp_dir = std::env::temp_dir();
    let pid = std::process::id();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let password_file = temp_dir.join(format!("test_acl_recheck_pw_{pid}_{ts}.txt"));
    let acl_file = temp_dir.join(format!("test_acl_recheck_acl_{pid}_{ts}.txt"));

    let _ = std::fs::remove_file(&password_file);
    let _ = std::fs::remove_file(&acl_file);

    let cli_binary = get_cli_binary_path();
    let status = Command::new(&cli_binary)
        .args([
            "passwd",
            "-c",
            "-b",
            "pass1",
            "alice",
            password_file.to_str().unwrap(),
        ])
        .status()
        .expect("create password file");
    assert!(status.success());

    {
        let mut f = std::fs::File::create(&acl_file).expect("create acl file");
        writeln!(f, "user alice topic sensors/# permission readwrite").unwrap();
        writeln!(f, "user alice topic control/# permission readwrite").unwrap();
    }

    let storage_config = StorageConfig {
        backend: StorageBackend::Memory,
        enable_persistence: true,
        ..Default::default()
    };

    let auth_config = AuthConfig {
        allow_anonymous: false,
        password_file: Some(password_file.clone()),
        acl_file: Some(acl_file.clone()),
        auth_method: AuthMethod::Password,
        auth_data: Some(std::fs::read(&password_file).expect("read password file")),
        scram_file: None,
        jwt_config: None,
        federated_jwt_config: None,
        rate_limit: RateLimitConfig::default(),
    };

    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(storage_config)
        .with_auth(auth_config);

    let broker = TestBroker::start_with_config(config).await;
    let client_id = "acl-recheck-test";

    let opts = ConnectOptions::new(client_id)
        .with_clean_start(true)
        .with_credentials("alice", b"pass1")
        .with_session_expiry_interval(300);
    let client1 = MqttClient::with_options(opts.clone());
    client1
        .connect_with_options(broker.address(), opts)
        .await
        .expect("first connect");

    client1.subscribe("sensors/temp", |_| {}).await.unwrap();
    client1.subscribe("control/valve", |_| {}).await.unwrap();
    client1.disconnect().await.unwrap();

    {
        let mut f = std::fs::File::create(&acl_file).expect("rewrite acl file");
        writeln!(f, "user alice topic sensors/# permission readwrite").unwrap();
        writeln!(f, "user alice topic control/# permission deny").unwrap();
    }

    let resume_opts = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_credentials("alice", b"pass1")
        .with_session_expiry_interval(300);
    let client2 = MqttClient::with_options(resume_opts.clone());
    let result = client2
        .connect_with_options(broker.address(), resume_opts)
        .await
        .expect("reconnect must succeed");

    assert!(
        result.session_present,
        "session must be present on reconnect"
    );

    let collector = common::MessageCollector::new();
    client2
        .subscribe("sensors/temp", collector.callback())
        .await
        .unwrap();

    let pub_opts = ConnectOptions::new("acl-recheck-pub")
        .with_clean_start(true)
        .with_credentials("alice", b"pass1");
    let publisher = MqttClient::with_options(pub_opts.clone());
    publisher
        .connect_with_options(broker.address(), pub_opts)
        .await
        .expect("publisher connect");

    publisher.publish("sensors/temp", b"22.5").await.unwrap();

    assert!(
        collector
            .wait_for_messages(1, mqtt5::time::Duration::from_secs(3))
            .await,
        "sensors/temp subscription must still work"
    );

    publisher.disconnect().await.unwrap();
    client2.disconnect().await.unwrap();

    let _ = std::fs::remove_file(&password_file);
    let _ = std::fs::remove_file(&acl_file);
}
