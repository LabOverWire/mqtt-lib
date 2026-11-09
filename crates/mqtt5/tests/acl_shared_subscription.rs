#![cfg(feature = "broker")]

use mqtt5::broker::acl::{AclManager, AclRule, Permission};
use mqtt5::broker::auth::{ComprehensiveAuthProvider, PasswordAuthProvider};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::MqttClient;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::QoS;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

async fn broker(acl: AclManager) -> (String, Arc<MessageRouter>) {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        });
    let provider = ComprehensiveAuthProvider::with_providers(
        PasswordAuthProvider::new().with_anonymous(true),
        acl,
    );
    let mut broker = MqttBroker::with_config(config)
        .await
        .unwrap()
        .with_auth_provider(Arc::new(provider));
    let url = format!("mqtt://{}", broker.local_addr().unwrap());
    let router = broker.router();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    (url, router)
}

async fn received_after_subscribing(
    url: &str,
    router: &MessageRouter,
    filter: &str,
    topic: &str,
) -> Vec<String> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = MqttClient::new("shared-subscriber");
    client.connect(url).await.unwrap();
    let sink = Arc::clone(&seen);
    let _ = client
        .subscribe(filter, move |message| {
            sink.lock().unwrap().push(message.topic.clone());
        })
        .await;
    let publish = PublishPacket::new(topic, &b"x"[..], QoS::AtLeastOnce);
    router.route_message(&publish, None).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let received = seen.lock().unwrap().clone();
    received
}

#[tokio::test]
async fn a_deny_rule_keeps_a_shared_subscriber_from_a_topic() {
    let acl = AclManager::allow_all();
    acl.add_rule(AclRule::new(
        "*".to_string(),
        "secret/#".to_string(),
        Permission::Deny,
    ))
    .await;
    let (url, router) = broker(acl).await;
    let received = received_after_subscribing(&url, &router, "$share/g/secret/x", "secret/x").await;
    assert!(received.is_empty(), "{received:?}");
}

#[tokio::test]
async fn an_allow_rule_lets_a_shared_subscriber_receive_a_topic() {
    let acl = AclManager::new();
    acl.add_rule(AclRule::new(
        "*".to_string(),
        "open/#".to_string(),
        Permission::ReadWrite,
    ))
    .await;
    let (url, router) = broker(acl).await;
    let received = received_after_subscribing(&url, &router, "$share/g/open/x", "open/x").await;
    assert_eq!(received, ["open/x"]);
}
