#![cfg(feature = "broker")]

use mqtt5::broker::acl::{AclManager, AclRule, Permission};
use mqtt5::broker::auth::{
    AuthProvider, AuthorizationMode, CompositeAuthProvider, ComprehensiveAuthProvider,
    PasswordAuthProvider,
};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, SubscribeOptions};
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::QoS;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn config() -> BrokerConfig {
    BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        })
}

async fn start(auth: Arc<dyn AuthProvider>) -> (SocketAddr, Arc<MessageRouter>) {
    let mut broker = MqttBroker::with_config(config())
        .await
        .unwrap()
        .with_auth_provider(auth);
    let addr = broker.local_addr().unwrap();
    let router = broker.router();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (addr, router)
}

fn comprehensive(acl: AclManager) -> Arc<ComprehensiveAuthProvider> {
    Arc::new(ComprehensiveAuthProvider::with_providers(
        PasswordAuthProvider::new().with_anonymous(true),
        acl,
    ))
}

async fn read_all_write_narrow() -> AclManager {
    let acl = AclManager::new();
    acl.add_rule(AclRule::new("*", "data/#", Permission::Read))
        .await;
    acl.add_rule(AclRule::new("*", "data/in/#", Permission::Write))
        .await;
    acl
}

async fn received(auth: Arc<dyn AuthProvider>, id: &str) -> Vec<String> {
    let (addr, router) = start(auth).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = MqttClient::new(id);
    client
        .connect_with_options(
            &format!("mqtt://{addr}"),
            ConnectOptions::new(id).with_clean_start(true),
        )
        .await
        .unwrap();
    let sink = Arc::clone(&seen);
    client
        .subscribe_with_options(
            "data/#",
            SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |message| sink.lock().unwrap().push(message.topic.clone()),
        )
        .await
        .unwrap();
    for topic in ["data/in/x", "data/open"] {
        let publish = PublishPacket::new(topic, &b"x"[..], QoS::AtLeastOnce);
        router.route_message(&publish, None).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got = seen.lock().unwrap().clone();
    got
}

#[tokio::test]
async fn an_acl_alone_delivers_under_a_narrower_write_rule() {
    let got = received(comprehensive(read_all_write_narrow().await), "alone").await;
    assert_eq!(got, ["data/in/x", "data/open"]);
}

#[tokio::test]
async fn or_with_a_provider_granting_nothing_delivers_the_same_as_the_acl_alone() {
    let composite = CompositeAuthProvider::new(
        comprehensive(read_all_write_narrow().await),
        comprehensive(AclManager::new()),
    )
    .with_authorization_mode(AuthorizationMode::Or);
    let got = received(Arc::new(composite), "or-nothing").await;
    assert_eq!(got, ["data/in/x", "data/open"]);
}

#[tokio::test]
async fn or_with_itself_delivers_the_same_as_the_acl_alone() {
    let composite = CompositeAuthProvider::new(
        comprehensive(read_all_write_narrow().await),
        comprehensive(read_all_write_narrow().await),
    )
    .with_authorization_mode(AuthorizationMode::Or);
    let got = received(Arc::new(composite), "or-self").await;
    assert_eq!(got, ["data/in/x", "data/open"]);
}
