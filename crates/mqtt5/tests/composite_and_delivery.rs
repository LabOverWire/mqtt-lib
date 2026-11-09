#![cfg(feature = "broker")]

use mqtt5::broker::acl::{AclManager, AclRule, Permission};
use mqtt5::broker::auth::{
    AuthProvider, AuthResult, AuthorizationMode, CompositeAuthProvider, ComprehensiveAuthProvider,
    PasswordAuthProvider,
};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::error::Result;
use mqtt5::{ConnectOptions, MqttClient, SubscribeOptions};
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::QoS;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
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

struct NoDeliveryCheck;

impl AuthProvider for NoDeliveryCheck {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async { Ok(AuthResult::success()) })
    }
    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }
    fn authorize_delivery<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { false })
    }
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
    for topic in ["data/open"] {
        let publish = PublishPacket::new(topic, &b"x"[..], QoS::AtLeastOnce);
        router.route_message(&publish, None).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got = seen.lock().unwrap().clone();
    got
}

async fn acl_provider() -> Arc<ComprehensiveAuthProvider> {
    let acl = AclManager::new();
    acl.add_rule(AclRule::new("*", "data/#", Permission::Read))
        .await;
    Arc::new(ComprehensiveAuthProvider::with_providers(
        PasswordAuthProvider::new().with_anonymous(true),
        acl,
    ))
}

async fn composite(mode: AuthorizationMode) -> Arc<dyn AuthProvider> {
    Arc::new(
        CompositeAuthProvider::new(acl_provider().await, Arc::new(NoDeliveryCheck))
            .with_authorization_mode(mode),
    )
}

#[tokio::test]
async fn a_provider_that_doesnt_check_delivery_delivers_alone() {
    let got = received(Arc::new(NoDeliveryCheck), "alone").await;
    assert_eq!(got, ["data/open"]);
}

#[tokio::test]
async fn or_mode_with_a_provider_that_doesnt_check_delivery_delivers() {
    let got = received(composite(AuthorizationMode::Or).await, "or").await;
    assert_eq!(got, ["data/open"]);
}

#[tokio::test]
async fn and_mode_with_a_provider_that_doesnt_check_delivery_delivers() {
    let got = received(composite(AuthorizationMode::And).await, "and").await;
    assert_eq!(got, ["data/open"]);
}
