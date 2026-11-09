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

struct Running {
    url: String,
    router: Arc<MessageRouter>,
    provider: Arc<ComprehensiveAuthProvider>,
}

fn allowing_data() -> Arc<ComprehensiveAuthProvider> {
    Arc::new(ComprehensiveAuthProvider::with_providers(
        PasswordAuthProvider::new().with_anonymous(true),
        AclManager::allow_all(),
    ))
}

async fn start(auth: Arc<dyn AuthProvider>, provider: Arc<ComprehensiveAuthProvider>) -> Running {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        });
    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(auth);
    let url = format!("mqtt://{}", broker.local_addr().unwrap());
    let router = broker.router();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    Running {
        url,
        router,
        provider,
    }
}

async fn running() -> Running {
    let provider = allowing_data();
    start(provider.clone(), provider).await
}

impl Running {
    async fn deny_secret(&self) {
        self.provider
            .acl_manager()
            .add_rule(AclRule::new(
                "*".to_string(),
                "data/secret/#".to_string(),
                Permission::Deny,
            ))
            .await;
    }

    async fn route(&self, topic: &str, retain: bool) {
        let mut publish = PublishPacket::new(topic, &b"x"[..], QoS::AtLeastOnce);
        publish.retain = retain;
        self.router.route_message(&publish, None).await;
    }
}

async fn subscribed_to_data(url: &str, id: &str) -> (MqttClient, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = MqttClient::new(id);
    client
        .connect_with_options(
            url,
            ConnectOptions::new(id)
                .with_clean_start(true)
                .with_session_expiry_interval(3600),
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
            move |message| {
                sink.lock().unwrap().push(message.topic.clone());
            },
        )
        .await
        .unwrap();
    (client, seen)
}

async fn settled(seen: &Mutex<Vec<String>>) -> Vec<String> {
    tokio::time::sleep(Duration::from_millis(300)).await;
    seen.lock().unwrap().clone()
}

#[tokio::test]
async fn a_deny_rule_under_a_wildcard_keeps_a_live_message_from_a_subscriber() {
    let broker = running().await;
    broker.deny_secret().await;
    let (_client, seen) = subscribed_to_data(&broker.url, "live").await;
    broker.route("data/secret/x", false).await;
    broker.route("data/open", false).await;
    assert_eq!(settled(&seen).await, ["data/open"]);
}

#[tokio::test]
async fn a_deny_rule_added_after_subscribing_applies() {
    let broker = running().await;
    let (_client, seen) = subscribed_to_data(&broker.url, "later").await;
    broker.route("data/secret/before", false).await;
    assert_eq!(settled(&seen).await, ["data/secret/before"]);
    broker.deny_secret().await;
    broker.route("data/secret/after", false).await;
    assert_eq!(settled(&seen).await, ["data/secret/before"]);
}

#[tokio::test]
async fn a_deny_rule_under_a_wildcard_keeps_a_retained_message_from_a_subscriber() {
    let broker = running().await;
    broker.route("data/secret/retained", true).await;
    broker.route("data/open", true).await;
    broker.deny_secret().await;
    let (_client, seen) = subscribed_to_data(&broker.url, "retained").await;
    assert_eq!(settled(&seen).await, ["data/open"]);
}

#[tokio::test]
async fn a_deny_rule_under_a_wildcard_keeps_a_queued_message_from_a_subscriber() {
    let broker = running().await;
    let (client, seen) = subscribed_to_data(&broker.url, "queued").await;
    client.disconnect().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    broker.route("data/secret/x", false).await;
    broker.route("data/open", false).await;
    broker.deny_secret().await;
    client
        .connect_with_options(
            &broker.url,
            ConnectOptions::new("queued")
                .with_clean_start(false)
                .with_session_expiry_interval(3600),
        )
        .await
        .unwrap();
    assert_eq!(settled(&seen).await, ["data/open"]);
}

#[tokio::test]
async fn a_composite_provider_applies_its_primary_acl_at_delivery() {
    let provider = allowing_data();
    let fallback = Arc::new(PasswordAuthProvider::new().with_anonymous(true));
    let composite = Arc::new(
        CompositeAuthProvider::new(provider.clone(), fallback)
            .with_authorization_mode(AuthorizationMode::PrimaryOnly),
    );
    let broker = start(composite, provider).await;
    broker.deny_secret().await;
    let (_client, seen) = subscribed_to_data(&broker.url, "composite").await;
    broker.route("data/secret/x", false).await;
    broker.route("data/open", false).await;
    assert_eq!(settled(&seen).await, ["data/open"]);
}
