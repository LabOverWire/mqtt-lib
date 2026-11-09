#![cfg(feature = "broker")]

use mqtt5::broker::acl::{AclManager, AclRule, Permission};
use mqtt5::broker::auth::{ComprehensiveAuthProvider, PasswordAuthProvider};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::MqttClient;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

async fn received_with_read_rule(pattern: &str, filter: &str) -> Vec<String> {
    let acl = AclManager::new();
    acl.add_rule(AclRule::new("*", pattern, Permission::Read))
        .await;
    acl.add_rule(AclRule::new("anonymous", "#", Permission::Write))
        .await;
    let provider = Arc::new(ComprehensiveAuthProvider::with_providers(
        PasswordAuthProvider::new().with_anonymous(true),
        acl,
    ));
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
        .with_auth_provider(provider);
    let url = format!("mqtt://{}", broker.local_addr().unwrap());
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();

    let received = Arc::new(Mutex::new(Vec::new()));
    let subscriber = MqttClient::new("coverage-sub");
    subscriber.connect(&url).await.unwrap();
    let sink = Arc::clone(&received);
    let _ = subscriber
        .subscribe(filter, move |message| {
            sink.lock().unwrap().push(message.topic.clone());
        })
        .await;
    let publisher = MqttClient::new("coverage-pub");
    publisher.connect(&url).await.unwrap();
    for topic in ["a/b/c", "a/b/c/d"] {
        publisher.publish_qos1(topic, b"x".to_vec()).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let topics = received.lock().unwrap().clone();
    topics
}

#[tokio::test]
async fn a_single_level_read_rule_does_not_let_a_multi_level_filter_read_deeper() {
    assert!(received_with_read_rule("a/+", "a/#").await.is_empty());
}

#[tokio::test]
async fn control_a_multi_level_read_rule_lets_the_filter_read_deeper() {
    assert_eq!(
        received_with_read_rule("a/#", "a/#").await,
        ["a/b/c", "a/b/c/d"]
    );
}
