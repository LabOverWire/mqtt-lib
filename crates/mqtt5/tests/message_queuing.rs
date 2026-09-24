use mqtt5::{ConnectOptions, MqttClient, PublishOptions, PublishResult, QoS};

#[tokio::test]
async fn test_message_queuing_when_disconnected() {
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    assert!(client.is_queue_on_disconnect().await);

    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/topic", "queued message", options)
        .await;

    assert!(result.is_ok());
    match result.unwrap() {
        PublishResult::Queued(handle) => assert_eq!(handle.try_outcome(), None),
        other @ PublishResult::Sent(_) => panic!("expected a queued publish, got {other:?}"),
    }
}

#[tokio::test]
async fn test_message_queuing_disabled() {
    let options = ConnectOptions::new("test-client").with_clean_start(true);
    let client = MqttClient::with_options(options);

    assert!(!client.is_queue_on_disconnect().await);

    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/topic", "message", options)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_qos0_not_queued() {
    let client = MqttClient::new("test-client");

    let options = PublishOptions::default();

    let result = client
        .publish_with_options("test/topic", "qos0 message", options)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_queue_multiple_messages() {
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    let mut handles = Vec::new();

    for i in 0..5 {
        let options = PublishOptions {
            qos: QoS::AtLeastOnce,
            ..Default::default()
        };

        let result = client
            .publish_with_options(format!("test/topic/{i}"), format!("message {i}"), options)
            .await;

        assert!(result.is_ok());
        match result.unwrap() {
            PublishResult::Queued(handle) => handles.push(handle),
            other @ PublishResult::Sent(_) => panic!("expected a queued publish, got {other:?}"),
        }
    }

    assert_eq!(handles.len(), 5);
    assert!(handles.iter().all(|handle| handle.try_outcome().is_none()));
}

#[tokio::test]
async fn test_toggle_queue_on_disconnect() {
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    assert!(client.is_queue_on_disconnect().await);

    client.set_queue_on_disconnect(false).await;
    assert!(!client.is_queue_on_disconnect().await);

    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    let result = client
        .publish_with_options("test/topic", "message", options)
        .await;
    assert!(result.is_err());

    client.set_queue_on_disconnect(true).await;
    assert!(client.is_queue_on_disconnect().await);

    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };
    let result = client
        .publish_with_options("test/topic", "message", options)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_message_replay_on_reconnect() {
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    let messages = vec![
        ("test/1", "message 1", QoS::AtLeastOnce),
        ("test/2", "message 2", QoS::ExactlyOnce),
        ("test/3", "message 3", QoS::AtLeastOnce),
    ];

    let mut handles = Vec::new();

    for (topic, payload, qos) in messages {
        let options = PublishOptions {
            qos,
            ..Default::default()
        };

        let result = client.publish_with_options(topic, payload, options).await;
        assert!(result.is_ok());
        match result.unwrap() {
            PublishResult::Queued(handle) => handles.push(handle),
            other @ PublishResult::Sent(_) => panic!("expected a queued publish, got {other:?}"),
        }
    }

    assert_eq!(handles.len(), 3);
}

#[tokio::test]
async fn test_retained_message_queuing() {
    let options = ConnectOptions::new("test-client").with_clean_start(false);
    let client = MqttClient::with_options(options);

    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        retain: true,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/retained", "retained message", options)
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_clean_session_no_queuing() {
    let options = ConnectOptions::new("clean-client").with_clean_start(true);
    let client = MqttClient::with_options(options);

    assert!(!client.is_queue_on_disconnect().await);

    client.set_queue_on_disconnect(true).await;
    assert!(client.is_queue_on_disconnect().await);

    let pub_opts = PublishOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    };

    let result = client
        .publish_with_options("test/topic", "message", pub_opts)
        .await;
    assert!(result.is_ok());
}
