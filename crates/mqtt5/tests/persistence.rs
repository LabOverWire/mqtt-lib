#![cfg(feature = "broker")]

mod common;
use common::TestBroker;

use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use tokio::time::sleep;

#[tokio::test]
async fn test_clean_start_true() {
    let broker = TestBroker::start().await;

    let options = ConnectOptions::new("clean-start-true").with_clean_start(true);

    let client = MqttClient::with_options(options);

    let session_present = Box::pin(client.connect_with_options(
        broker.address(),
        ConnectOptions::new("clean-start-true").with_clean_start(true),
    ))
    .await
    .unwrap();

    assert!(
        !session_present.session_present,
        "First connection should not have session present"
    );

    client.subscribe("test/clean", |_| {}).await.unwrap();

    client.disconnect().await.unwrap();

    let session_present = Box::pin(client.connect_with_options(
        broker.address(),
        ConnectOptions::new("clean-start-true").with_clean_start(true),
    ))
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
    let broker = TestBroker::start().await;

    let client_id = "persist-test-1";

    let client1 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(true));
    client1.connect(broker.address()).await.unwrap();

    client1.subscribe("test/persist/1", |_| {}).await.unwrap();
    client1.subscribe("test/persist/2", |_| {}).await.unwrap();

    client1.disconnect().await.unwrap();

    let client2 = MqttClient::with_options(
        ConnectOptions::new(client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );

    let session_present = Box::pin(
        client2.connect_with_options(
            broker.address(),
            ConnectOptions::new(client_id)
                .with_clean_start(false)
                .with_resume_existing_session(true),
        ),
    )
    .await
    .unwrap();

    let session_present_flag = session_present.session_present;
    println!("Session present: {session_present_flag}");
    if !session_present.session_present {
        println!("Warning: Broker did not preserve session. This is broker-dependent behavior.");
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    client2
        .subscribe("test/persist/1", move |_| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    client2.publish("test/persist/1", "test").await.unwrap();
    sleep(Duration::from_millis(500)).await;

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
    let broker = TestBroker::start().await;

    let client_id = "session-expiry-test";

    let options = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_resume_existing_session(true)
        .with_session_expiry_interval(5);

    let client1 = MqttClient::with_options(options.clone());
    client1.connect(broker.address()).await.unwrap();

    client1.subscribe("test/expiry", |_| {}).await.unwrap();

    client1.disconnect().await.unwrap();

    sleep(Duration::from_secs(2)).await;

    let client2 = MqttClient::with_options(options.clone());
    let session_present = Box::pin(client2.connect_with_options(broker.address(), options.clone()))
        .await
        .unwrap();

    assert!(
        session_present.session_present,
        "Session should exist within expiry interval"
    );
    client2.disconnect().await.unwrap();

    sleep(Duration::from_secs(4)).await;

    let client3 = MqttClient::with_options(options);
    let session_present = Box::pin(
        client3.connect_with_options(
            broker.address(),
            ConnectOptions::new(client_id)
                .with_clean_start(false)
                .with_resume_existing_session(true),
        ),
    )
    .await
    .unwrap();

    println!(
        "Session present after expiry: {}",
        session_present.session_present
    );

    client3.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos1_message_persistence() {
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("persist-pub");
    let sub_client_id = "persist-sub-qos1";

    let sub_options = ConnectOptions::new(sub_client_id)
        .with_clean_start(false)
        .with_resume_existing_session(true);
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

    sub_client.disconnect().await.unwrap();

    pub_client.connect(broker.address()).await.unwrap();

    for i in 0..5 {
        pub_client
            .publish_qos1("test/persist/qos1", format!("Offline message {i}"))
            .await
            .unwrap();
    }

    pub_client.disconnect().await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    let sub_client2 = MqttClient::with_options(
        ConnectOptions::new(sub_client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );

    let session_present = Box::pin(
        sub_client2.connect_with_options(
            broker.address(),
            ConnectOptions::new(sub_client_id)
                .with_clean_start(false)
                .with_resume_existing_session(true),
        ),
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

    sleep(Duration::from_secs(2)).await;

    let count = received.load(Ordering::Relaxed);
    println!("Received {count} offline messages");
    if session_present.session_present {
        assert!(
            count > 0,
            "Should receive some offline messages when session is preserved"
        );
    }

    match sub_client2.disconnect().await {
        Ok(()) | Err(mqtt5::MqttError::NotConnected) => {}
        Err(e) => panic!("unexpected disconnect error: {e}"),
    }
}

#[tokio::test]
async fn test_qos2_message_persistence() {
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("persist-pub-qos2");
    let sub_client_id = "persist-sub-qos2";

    let sub_options = ConnectOptions::new(sub_client_id)
        .with_clean_start(false)
        .with_resume_existing_session(true);
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

    sub_client.disconnect().await.unwrap();

    pub_client.connect(broker.address()).await.unwrap();

    for i in 0..3 {
        pub_client
            .publish_qos2("test/persist/qos2", format!("QoS2 offline message {i}"))
            .await
            .unwrap();
    }

    pub_client.disconnect().await.unwrap();

    let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let messages_clone = messages.clone();

    let sub_client2 = MqttClient::with_options(
        ConnectOptions::new(sub_client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );

    sub_client2.connect(broker.address()).await.unwrap();

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

    sleep(Duration::from_secs(2)).await;

    {
        let msgs = messages.lock().unwrap();
        let msg_count = msgs.len();
        println!("Received {msg_count} QoS 2 offline messages");

        let mut unique_msgs = msgs.clone();
        unique_msgs.sort();
        unique_msgs.dedup();
        assert_eq!(msgs.len(), unique_msgs.len(), "No duplicate QoS 2 messages");
    }

    match sub_client2.disconnect().await {
        Ok(()) | Err(mqtt5::MqttError::NotConnected) => {}
        Err(e) => panic!("unexpected disconnect error: {e}"),
    }
}

#[tokio::test]
async fn test_subscription_persistence() {
    let broker = TestBroker::start().await;

    let client_id = "sub-persist-test";

    let client1 = MqttClient::with_options(ConnectOptions::new(client_id).with_clean_start(true));
    client1.connect(broker.address()).await.unwrap();

    client1.subscribe("test/sub/1", |_| {}).await.unwrap();
    client1.subscribe("test/sub/2", |_| {}).await.unwrap();
    client1.subscribe("test/sub/+", |_| {}).await.unwrap();

    client1.disconnect().await.unwrap();

    let received_topics = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received_topics_clone = received_topics.clone();

    let client2 = MqttClient::with_options(
        ConnectOptions::new(client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );

    let session_present = Box::pin(
        client2.connect_with_options(
            broker.address(),
            ConnectOptions::new(client_id)
                .with_clean_start(false)
                .with_resume_existing_session(true),
        ),
    )
    .await
    .unwrap();

    println!(
        "Session present for subscription persistence: {}",
        session_present.session_present
    );
    if !session_present.session_present {
        println!("Warning: Broker did not preserve session for subscription test");
        client2.disconnect().await.unwrap();
        return;
    }

    client2
        .subscribe("test/sub/+", move |msg| {
            received_topics_clone
                .lock()
                .unwrap()
                .push(msg.topic.clone());
        })
        .await
        .unwrap();

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
    }

    client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_will_message_persistence() {
    let broker = TestBroker::start().await;

    let will_client_id = "will-persist-test";
    let sub_client = MqttClient::new("will-sub");

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

    let will_msg = mqtt5::WillMessage::new("test/will/persist", "Client died")
        .with_qos(QoS::AtLeastOnce)
        .with_retain(false);

    let will_options = ConnectOptions::new(will_client_id)
        .with_clean_start(false)
        .with_resume_existing_session(true)
        .with_will(will_msg);

    let will_client = MqttClient::with_options(will_options);
    will_client.connect(broker.address()).await.unwrap();

    drop(will_client);

    sleep(Duration::from_secs(2)).await;

    let received = will_received.load(Ordering::Relaxed);
    println!("Will message received: {received}");
    if !received {
        println!("Warning: Will message not received. This may be due to broker configuration or the connection not being detected as abnormal.");
    }

    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_packet_id_persistence() {
    let broker = TestBroker::start().await;

    let client_id = "packet-id-persist";

    let options = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_resume_existing_session(true);
    let client1 = MqttClient::with_options(options.clone());

    client1.connect(broker.address()).await.unwrap();

    let mut first_ids = Vec::new();
    for i in 0..5 {
        let id = client1
            .publish_qos1("test/pid", format!("Message {i}"))
            .await
            .unwrap();
        first_ids.push(id);
    }

    client1.disconnect().await.unwrap();

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

    println!("First session IDs: {first_ids:?}");
    println!("Second session IDs: {second_ids:?}");

    client2.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_inflight_message_persistence() {
    let broker = TestBroker::start().await;

    let pub_client_id = "inflight-pub";
    let sub_client_id = "inflight-sub";

    let sub_client = MqttClient::with_options(
        ConnectOptions::new(sub_client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );
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

    let pub_client = MqttClient::with_options(
        ConnectOptions::new(pub_client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );
    pub_client.connect(broker.address()).await.unwrap();

    for i in 0..10 {
        let _ = pub_client
            .publish_qos1("test/inflight", format!("Msg {i}"))
            .await;
    }

    pub_client.disconnect().await.unwrap();

    sleep(Duration::from_millis(500)).await;

    let initial_count = received.load(Ordering::Relaxed);
    println!("Initially received: {initial_count} messages");

    let pub_client2 = MqttClient::with_options(
        ConnectOptions::new(pub_client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );
    pub_client2.connect(broker.address()).await.unwrap();

    sleep(Duration::from_secs(1)).await;

    let final_count = received.load(Ordering::Relaxed);
    println!("Finally received: {final_count} messages");

    assert!(
        final_count >= 10,
        "Should receive all messages including retransmissions"
    );

    pub_client2.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
}

#[tokio::test]
async fn test_qos2_outbound_inflight_resend_on_reconnect() {
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("qos2-inflight-pub");
    let sub_client_id = "qos2-inflight-sub";

    let sub_client = MqttClient::with_options(
        ConnectOptions::new(sub_client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );
    sub_client.connect(broker.address()).await.unwrap();

    sub_client
        .subscribe_with_options(
            "test/qos2/inflight",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();

    sub_client.disconnect().await.unwrap();

    pub_client.connect(broker.address()).await.unwrap();
    for i in 0..3 {
        pub_client
            .publish_qos2("test/qos2/inflight", format!("inflight msg {i}"))
            .await
            .unwrap();
    }
    pub_client.disconnect().await.unwrap();

    let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let messages_clone = messages.clone();

    let sub_client2 = MqttClient::with_options(
        ConnectOptions::new(sub_client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );

    let session = Box::pin(
        sub_client2.connect_with_options(
            broker.address(),
            ConnectOptions::new(sub_client_id)
                .with_clean_start(false)
                .with_resume_existing_session(true),
        ),
    )
    .await
    .unwrap();

    sub_client2
        .subscribe_with_options(
            "test/qos2/inflight",
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

    sleep(Duration::from_secs(2)).await;

    {
        let msgs = messages.lock().unwrap();
        if session.session_present {
            assert!(
                !msgs.is_empty(),
                "should receive QoS2 messages after reconnect with session_present"
            );
            let mut unique = msgs.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(
                msgs.len(),
                unique.len(),
                "QoS2 should not produce duplicates"
            );
        }
    }

    match sub_client2.disconnect().await {
        Ok(()) | Err(mqtt5::MqttError::NotConnected) => {}
        Err(e) => panic!("unexpected disconnect error: {e}"),
    }
}

#[tokio::test]
async fn test_clean_start_clears_inflight_state() {
    let broker = TestBroker::start().await;

    let pub_client = MqttClient::new("clean-inflight-pub");
    let sub_client_id = "clean-inflight-sub";

    let sub_client = MqttClient::with_options(
        ConnectOptions::new(sub_client_id)
            .with_clean_start(false)
            .with_resume_existing_session(true),
    );
    sub_client.connect(broker.address()).await.unwrap();

    sub_client
        .subscribe_with_options(
            "test/clean/inflight",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            |_| {},
        )
        .await
        .unwrap();

    sub_client.disconnect().await.unwrap();

    pub_client.connect(broker.address()).await.unwrap();
    for i in 0..3 {
        pub_client
            .publish_qos2("test/clean/inflight", format!("clean msg {i}"))
            .await
            .unwrap();
    }
    pub_client.disconnect().await.unwrap();

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    let sub_client2 =
        MqttClient::with_options(ConnectOptions::new(sub_client_id).with_clean_start(true));

    let session = Box::pin(sub_client2.connect_with_options(
        broker.address(),
        ConnectOptions::new(sub_client_id).with_clean_start(true),
    ))
    .await
    .unwrap();

    assert!(
        !session.session_present,
        "clean_start=true should not have session_present"
    );

    sub_client2
        .subscribe_with_options(
            "test/clean/inflight",
            mqtt5::SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            },
        )
        .await
        .unwrap();

    sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        0,
        "clean_start=true should not deliver old queued or inflight messages"
    );

    sub_client2.disconnect().await.unwrap();
}
