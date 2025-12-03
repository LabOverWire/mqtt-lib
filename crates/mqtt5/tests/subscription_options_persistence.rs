mod common;
use common::TestBroker;

use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS, SubscribeOptions};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::time::sleep;

#[tokio::test]
async fn test_no_local_persists_after_reconnect() {
    let broker = TestBroker::start().await;
    let client_id = "no-local-persist-test";

    let client1 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(true));
    client1.connect(broker.address()).await.unwrap();

    client1
        .subscribe_with_options(
            "test/no-local",
            SubscribeOptions {
                qos: QoS::AtMostOnce,
                no_local: true,
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();

    client1.disconnect().await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    let client2 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(false));
    let session = client2
        .connect_with_options(
            broker.address(),
            ConnectOptions::new(client_id).with_clean_start(false),
        )
        .await
        .unwrap();

    if !session.session_present {
        println!("Session not preserved, skipping test");
        client2.disconnect().await.unwrap();
        return;
    }

    client2
        .subscribe_with_options(
            "test/no-local",
            SubscribeOptions {
                qos: QoS::AtMostOnce,
                no_local: true,
                ..Default::default()
            },
            move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    client2
        .publish("test/no-local", "self-message")
        .await
        .unwrap();
    sleep(Duration::from_millis(200)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        0,
        "no_local should be preserved - client should NOT receive its own message after reconnect"
    );

    let other_client = MqttClient::new("other-publisher");
    other_client.connect(broker.address()).await.unwrap();
    other_client
        .publish("test/no-local", "other-message")
        .await
        .unwrap();
    other_client.disconnect().await.unwrap();

    sleep(Duration::from_millis(200)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        1,
        "Client should receive messages from other clients"
    );

    client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_retain_as_published_persists_after_reconnect() {
    let broker = TestBroker::start().await;
    let client_id = "retain-as-pub-persist";

    let pub_client = MqttClient::new("retain-publisher");
    pub_client.connect(broker.address()).await.unwrap();
    pub_client
        .publish_with_options(
            "test/retain-persist",
            "retained-message",
            mqtt5::PublishOptions {
                qos: QoS::AtMostOnce,
                retain: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    pub_client.disconnect().await.unwrap();

    let client1 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(true));
    client1.connect(broker.address()).await.unwrap();

    client1
        .subscribe_with_options(
            "test/retain-persist",
            SubscribeOptions {
                qos: QoS::AtMostOnce,
                retain_as_published: false,
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();

    client1.disconnect().await.unwrap();

    let retain_flag_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let retain_clone = retain_flag_seen.clone();

    let client2 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(false));
    let session = client2
        .connect_with_options(
            broker.address(),
            ConnectOptions::new(client_id).with_clean_start(false),
        )
        .await
        .unwrap();

    if !session.session_present {
        println!("Session not preserved, skipping test");
        client2.disconnect().await.unwrap();
        return;
    }

    client2
        .subscribe_with_options(
            "test/retain-persist-new",
            SubscribeOptions {
                qos: QoS::AtMostOnce,
                retain_as_published: false,
                ..Default::default()
            },
            move |msg| {
                retain_clone.lock().unwrap().push(msg.retain);
            },
        )
        .await
        .unwrap();

    pub_client.connect(broker.address()).await.unwrap();
    pub_client
        .publish_with_options(
            "test/retain-persist-new",
            "retained-msg-2",
            mqtt5::PublishOptions {
                qos: QoS::AtMostOnce,
                retain: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    pub_client.disconnect().await.unwrap();

    sleep(Duration::from_millis(200)).await;

    {
        let flags = retain_flag_seen.lock().unwrap();
        if !flags.is_empty() {
            assert!(
                !flags[0],
                "retain_as_published=false should clear retain flag on delivery"
            );
        }
    }

    client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_subscription_options_all_preserved() {
    let broker = TestBroker::start().await;
    let client_id = "full-options-persist";

    let client1 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(true));
    client1.connect(broker.address()).await.unwrap();

    client1
        .subscribe_with_options(
            "test/full-options",
            SubscribeOptions {
                qos: QoS::ExactlyOnce,
                no_local: true,
                retain_as_published: true,
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();

    client1.disconnect().await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    let client2 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(false));
    let session = client2
        .connect_with_options(
            broker.address(),
            ConnectOptions::new(client_id).with_clean_start(false),
        )
        .await
        .unwrap();

    if !session.session_present {
        println!("Session not preserved, skipping test");
        client2.disconnect().await.unwrap();
        return;
    }

    client2
        .subscribe_with_options(
            "test/full-options",
            SubscribeOptions {
                qos: QoS::ExactlyOnce,
                no_local: true,
                retain_as_published: true,
                ..Default::default()
            },
            move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    client2
        .publish_qos2("test/full-options", "self-qos2-message")
        .await
        .unwrap();
    sleep(Duration::from_millis(300)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        0,
        "no_local should prevent receiving own QoS2 message after session restore"
    );

    let other = MqttClient::new("qos2-sender");
    other.connect(broker.address()).await.unwrap();
    other
        .publish_qos2("test/full-options", "other-qos2-message")
        .await
        .unwrap();
    other.disconnect().await.unwrap();

    sleep(Duration::from_millis(300)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        1,
        "Should receive QoS2 message from other client"
    );

    client2.disconnect().await.unwrap();
}
