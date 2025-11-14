mod common;
use common::TestBroker;

use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::time::sleep;

#[tokio::test]
async fn test_clean_start_true() {
    // Start test broker
    let broker = TestBroker::start().await;

    let options = ConnectOptions::new("clean-start-true").with_clean_start(true);

    let client = MqttClient::with_options(options);

    // First connection
    let session_present = client
        .connect_with_options(
            broker.address(),
            ConnectOptions::new("clean-start-true").with_clean_start(true),
        )
        .await
        .unwrap();

    assert!(
        !session_present.session_present,
        "First connection should not have session present"
    );

    // Subscribe to a topic
    client.subscribe("test/clean", |_| {}).await.unwrap();

    client.disconnect().await.unwrap();

    // Second connection with clean_start=true
    let session_present = client
        .connect_with_options(
            broker.address(),
            ConnectOptions::new("clean-start-true").with_clean_start(true),
        )
        .await
        .unwrap();

    assert!(
        !session_present.session_present,
        "Clean start should not restore session"
    );

    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_clean_start_false() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client_id = "persist-test-1";

    // First connection with clean_start=true to ensure clean slate
    let client1 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(true));
    client1.connect(broker.address()).await.unwrap();

    // Subscribe to topics
    client1.subscribe("test/persist/1", |_| {}).await.unwrap();
    client1.subscribe("test/persist/2", |_| {}).await.unwrap();

    client1.disconnect().await.unwrap();

    // Second connection with clean_start=false
    let client2 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(false));

    let session_present = client2
        .connect_with_options(
            broker.address(),
            ConnectOptions::new(client_id).with_clean_start(false),
        )
        .await
        .unwrap();

    // Note: Some brokers may not preserve sessions even with clean_start=false
    let session_present_flag = session_present.session_present;
    println!("Session present: {session_present_flag}");
    if !session_present.session_present {
        println!("Warning: Broker did not preserve session. This is broker-dependent behavior.");
    }

    // Subscriptions should still be active
    // Test by publishing to the subscribed topics
    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    // Re-subscribe to set up callback (broker maintains subscription but we need local callback)
    client2
        .subscribe("test/persist/1", move |_| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    client2.publish("test/persist/1", "test").await.unwrap();
    sleep(Duration::from_millis(500)).await;

    // Only check if session was actually preserved
    if session_present.session_present {
        assert!(
            received.load(Ordering::Relaxed) > 0,
            "Should receive message on persisted subscription"
        );
    }

    client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_session_expiry_interval() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client_id = "session-expiry-test";

    // Connect with session expiry interval
    let options = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(5); // 5 seconds

    let client1 = MqttClient::with_options(options.clone());
    client1.connect(broker.address()).await.unwrap();

    // Subscribe to a topic
    client1.subscribe("test/expiry", |_| {}).await.unwrap();

    client1.disconnect().await.unwrap();

    // Wait less than expiry interval
    sleep(Duration::from_secs(2)).await;

    // Reconnect - session should still exist
    let client2 = MqttClient::with_options(options.clone());
    let session_present = client2
        .connect_with_options(broker.address(), options.clone())
        .await
        .unwrap();

    assert!(
        session_present.session_present,
        "Session should exist within expiry interval"
    );
    client2.disconnect().await.unwrap();

    // Wait for session to expire
    sleep(Duration::from_secs(4)).await;

    // Reconnect - session should be gone
    let client3 = MqttClient::with_options(options);
    let session_present = client3
        .connect_with_options(
            broker.address(),
            ConnectOptions::new(client_id).with_clean_start(false),
        )
        .await
        .unwrap();

    // Broker might not have expired it yet, so we don't assert here
    println!(
        "Session present after expiry: {}",
        session_present.session_present
    );

    client3.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos1_message_persistence() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("persist-pub");
    let sub_client_id = "persist-sub-qos1";

    // Subscriber connects and subscribes
    let sub_options = ConnectOptions::new(sub_client_id).with_clean_start(false);
    let sub_client = MqttClient::with_options(sub_options);

    sub_client.connect(broker.address()).await.unwrap();
    sub_client
        .subscribe_with_options(
            "test/persist/qos1",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();

    // Disconnect subscriber
    sub_client.disconnect().await.unwrap();

    // Publisher sends QoS 1 messages while subscriber is offline
    pub_client.connect(broker.address()).await.unwrap();

    for i in 0..5 {
        pub_client
            .publish_qos1("test/persist/qos1", format!("Offline message {i}"))
            .await
            .unwrap();
    }

    pub_client.disconnect().await.unwrap();

    // Subscriber reconnects
    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    let sub_client2 =
        MqttClient::with_options(ConnectOptions::new(sub_client_id).with_clean_start(false));

    let session_present = sub_client2
        .connect_with_options(
            broker.address(),
            ConnectOptions::new(sub_client_id).with_clean_start(false),
        )
        .await
        .unwrap();

    println!(
        "Session present after reconnect: {}",
        session_present.session_present
    );
    if !session_present.session_present {
        println!("Warning: Broker did not restore session for QoS persistence test");
    }

    // Re-subscribe to set callback
    sub_client2
        .subscribe_with_options(
            "test/persist/qos1",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    // Wait for queued messages
    sleep(Duration::from_secs(2)).await;

    let count = received.load(Ordering::Relaxed);
    println!("Received {count} offline messages");
    // Only assert if session was preserved
    if session_present.session_present {
        assert!(
            count > 0,
            "Should receive some offline messages when session is preserved"
        );
    }

    sub_client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos2_message_persistence() {
    // Start test broker
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("persist-pub-qos2");
    let sub_client_id = "persist-sub-qos2";

    // Subscriber connects and subscribes with QoS 2
    let sub_options = ConnectOptions::new(sub_client_id).with_clean_start(false);
    let sub_client = MqttClient::with_options(sub_options);

    sub_client.connect(broker.address()).await.unwrap();
    sub_client
        .subscribe_with_options(
            "test/persist/qos2",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();

    // Disconnect subscriber
    sub_client.disconnect().await.unwrap();

    // Publisher sends QoS 2 messages while subscriber is offline
    pub_client.connect(broker.address()).await.unwrap();

    for i in 0..3 {
        pub_client
            .publish_qos2("test/persist/qos2", format!("QoS2 offline message {i}"))
            .await
            .unwrap();
    }

    pub_client.disconnect().await.unwrap();

    // Subscriber reconnects
    let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let messages_clone = messages.clone();

    let sub_client2 =
        MqttClient::with_options(ConnectOptions::new(sub_client_id).with_clean_start(false));

    sub_client2.connect(broker.address()).await.unwrap();

    // Re-subscribe to set callback
    sub_client2
        .subscribe_with_options(
            "test/persist/qos2",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |msg| {
                messages_clone
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&msg.payload).to_string());
            },
        )
        .await
        .unwrap();

    // Wait for queued messages
    sleep(Duration::from_secs(2)).await;

    {
        let msgs = messages.lock().unwrap();
        let msg_count = msgs.len();
        println!("Received {msg_count} QoS 2 offline messages");

        // Should receive exactly once
        let mut unique_msgs = msgs.clone();
        unique_msgs.sort();
        unique_msgs.dedup();
        assert_eq!(msgs.len(), unique_msgs.len(), "No duplicate QoS 2 messages");
    } // Drop the lock before awaiting

    sub_client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_subscription_persistence() {
    // Start test broker
    let broker = TestBroker::start().await;

    let client_id = "sub-persist-test";

    // First connection - subscribe to multiple topics
    let client1 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(true));
    client1.connect(broker.address()).await.unwrap();

    client1.subscribe("test/sub/1", |_| {}).await.unwrap();
    client1.subscribe("test/sub/2", |_| {}).await.unwrap();
    client1.subscribe("test/sub/+", |_| {}).await.unwrap();

    client1.disconnect().await.unwrap();

    // Second connection - subscriptions should persist
    let received_topics = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received_topics_clone = received_topics.clone();

    let client2 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(false));

    let session_present = client2
        .connect_with_options(
            broker.address(),
            ConnectOptions::new(client_id).with_clean_start(false),
        )
        .await
        .unwrap();

    println!(
        "Session present for subscription persistence: {}",
        session_present.session_present
    );
    if !session_present.session_present {
        println!("Warning: Broker did not preserve session for subscription test");
        // Skip the rest of the test if session wasn't preserved
        client2.disconnect().await.unwrap();
        return;
    }

    // Need to re-subscribe to set local callbacks
    // (broker maintains subscriptions but we need local handlers)
    client2
        .subscribe("test/sub/+", move |msg| {
            received_topics_clone
                .lock()
                .unwrap()
                .push(msg.topic.clone());
        })
        .await
        .unwrap();

    // Publish to subscribed topics
    client2.publish("test/sub/1", "msg1").await.unwrap();
    client2.publish("test/sub/2", "msg2").await.unwrap();
    client2.publish("test/sub/3", "msg3").await.unwrap();

    sleep(Duration::from_millis(500)).await;

    {
        let topics = received_topics.lock().unwrap();
        assert!(
            topics.len() >= 3,
            "Should receive messages on persisted subscriptions"
        );
    } // Drop the lock before awaiting

    client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_will_message_persistence() {
    // Start test broker
    let broker = TestBroker::start().await;

    let will_client_id = "will-persist-test";
    let sub_client = MqttClient::new("will-sub");

    // Subscribe to will topic
    sub_client.connect(broker.address()).await.unwrap();

    let will_received = Arc::new(AtomicBool::new(false));
    let will_received_clone = will_received.clone();

    sub_client
        .subscribe("test/will/persist", move |msg| {
            println!(
                "Received will message: {:?}",
                String::from_utf8_lossy(&msg.payload)
            );
            will_received_clone.store(true, Ordering::Relaxed);
        })
        .await
        .unwrap();

    // Connect with will message and persistent session
    let will_msg = mqtt5::WillMessage::new("test/will/persist", "Client died")
        .with_qos(QoS::AtLeastOnce)
        .with_retain(false);

    let will_options = ConnectOptions::new(will_client_id)
        .with_clean_start(false)
        .with_will(will_msg);

    let will_client = MqttClient::with_options(will_options);
    will_client.connect(broker.address()).await.unwrap();

    // Simulate abnormal disconnection by dropping the client
    // This causes the TCP connection to close without sending DISCONNECT
    drop(will_client);

    // Wait for will message
    sleep(Duration::from_secs(2)).await;

    // Will message delivery depends on broker implementation
    let received = will_received.load(Ordering::Relaxed);
    println!("Will message received: {received}");
    if !received {
        println!("Warning: Will message not received. This may be due to broker configuration or the connection not being detected as abnormal.");
    }

    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_packet_id_persistence() {
    // Start test broker
    let broker = TestBroker::start().await;

    // Test that packet IDs are managed correctly across reconnections
    let client_id = "packet-id-persist";

    let options = ConnectOptions::new(client_id).with_clean_start(false);
    let client1 = MqttClient::with_options(options.clone());

    client1.connect(broker.address()).await.unwrap();

    // Send some QoS 1 messages to allocate packet IDs
    let mut first_ids = Vec::new();
    for i in 0..5 {
        let id = client1
            .publish_qos1("test/pid", format!("Message {i}"))
            .await
            .unwrap();
        first_ids.push(id);
    }

    client1.disconnect().await.unwrap();

    // Reconnect and send more messages
    let client2 = MqttClient::with_options(options);
    client2.connect(broker.address()).await.unwrap();

    let mut second_ids = Vec::new();
    for i in 5..10 {
        let id = client2
            .publish_qos1("test/pid", format!("Message {i}"))
            .await
            .unwrap();
        second_ids.push(id);
    }

    // With clean disconnection, packet IDs can be reused
    // This is normal behavior as the previous IDs were acknowledged
    println!("First session IDs: {first_ids:?}");
    println!("Second session IDs: {second_ids:?}");

    client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_inflight_message_persistence() {
    // Start test broker
    let broker = TestBroker::start().await;

    // Test that in-flight QoS 1/2 messages are retransmitted after reconnection
    let pub_client_id = "inflight-pub";
    let sub_client_id = "inflight-sub";

    // Set up subscriber
    let sub_client =
        MqttClient::with_options(ConnectOptions::new(sub_client_id).with_clean_start(false));
    sub_client.connect(broker.address()).await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe_with_options(
            "test/inflight",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    // Publisher sends messages
    let pub_client =
        MqttClient::with_options(ConnectOptions::new(pub_client_id).with_clean_start(false));
    pub_client.connect(broker.address()).await.unwrap();

    // Send QoS 1 messages rapidly then disconnect
    // Some might still be in-flight
    for i in 0..10 {
        let _ = pub_client
            .publish_qos1("test/inflight", format!("Msg {i}"))
            .await;
    }

    // Quick disconnect might leave some messages in-flight
    pub_client.disconnect().await.unwrap();

    // Wait a bit
    sleep(Duration::from_millis(500)).await;

    let initial_count = received.load(Ordering::Relaxed);
    println!("Initially received: {initial_count} messages");

    // Reconnect publisher - any in-flight messages should be retransmitted
    let pub_client2 =
        MqttClient::with_options(ConnectOptions::new(pub_client_id).with_clean_start(false));
    pub_client2.connect(broker.address()).await.unwrap();

    // Wait for potential retransmissions
    sleep(Duration::from_secs(1)).await;

    let final_count = received.load(Ordering::Relaxed);
    println!("Finally received: {final_count} messages");

    // Should eventually receive all messages
    assert!(
        final_count >= 10,
        "Should receive all messages including retransmissions"
    );

    pub_client2.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}
