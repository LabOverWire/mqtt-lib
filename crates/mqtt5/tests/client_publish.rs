use mqtt5::{Delivery, MqttClient, MqttError, PublishOptions, PublishResult, QoS};
use ulid::Ulid;

/// Generate a lexicographically sortable client ID using ULID
fn test_client_id(test_name: &str) -> String {
    format!("test-{test_name}-{}", Ulid::new())
}

#[tokio::test]
async fn test_publish_not_connected() {
    let client = MqttClient::new(test_client_id("publish-not-connected"));

    let result = client.publish("test/topic", b"hello").await;
    assert!(matches!(result, Err(MqttError::NotConnected)));
}

#[tokio::test]
async fn test_publish_qos0() {
    let client = MqttClient::new(test_client_id("publish-qos0"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let result = client.publish_qos0("test/topic", b"QoS 0 message").await;
            assert!(result.is_ok());

            let result = client.publish("test/topic", b"Another QoS 0").await;
            assert!(matches!(
                result,
                Ok(PublishResult::Sent(Delivery::Unconfirmed))
            ));

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_publish_qos1() {
    let client = MqttClient::new(test_client_id("publish-qos1"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let result = client.publish_qos1("test/qos1", b"QoS 1 message").await;
            assert!(result.is_ok());
            match result.unwrap() {
                PublishResult::Sent(
                    Delivery::AtLeastOnce { packet_id } | Delivery::ExactlyOnce { packet_id },
                ) => assert!(packet_id > 0),
                other => panic!("expected an acknowledged publish, got {other:?}"),
            }

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_publish_qos2() {
    let client = MqttClient::new(test_client_id("publish-qos2"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let result = client.publish_qos2("test/qos2", b"QoS 2 message").await;
            assert!(result.is_ok());
            match result.unwrap() {
                PublishResult::Sent(
                    Delivery::AtLeastOnce { packet_id } | Delivery::ExactlyOnce { packet_id },
                ) => assert!(packet_id > 0),
                other => panic!("expected an acknowledged publish, got {other:?}"),
            }

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_publish_retain() {
    let client = MqttClient::new(test_client_id("publish-retain"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let result = client
                .publish_retain("test/retained", b"Retained message")
                .await;
            assert!(result.is_ok());

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_publish_with_options() {
    let client = MqttClient::new(test_client_id("publish-with-options"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let mut options = PublishOptions {
                qos: QoS::AtLeastOnce,
                retain: true,
                ..Default::default()
            };
            options.properties.content_type = Some("text/plain".to_string());
            options
                .properties
                .user_properties
                .push(("key".to_string(), "value".to_string()));

            let result = client
                .publish_with_options("test/options", b"Message with options", options)
                .await;

            assert!(result.is_ok());
            match result.unwrap() {
                PublishResult::Sent(
                    Delivery::AtLeastOnce { packet_id } | Delivery::ExactlyOnce { packet_id },
                ) => assert!(packet_id > 0),
                other => panic!("expected an acknowledged publish, got {other:?}"),
            }

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_publish_empty_payload() {
    let client = MqttClient::new(test_client_id("publish-empty"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let result = client.publish("test/empty", b"").await;
            assert!(result.is_ok());

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}

#[tokio::test]
async fn test_publish_string_conversion() {
    let client = MqttClient::new(test_client_id("publish-string"));

    match client.connect("mqtt://127.0.0.1:1883").await {
        Ok(()) => {
            let result = client.publish("test/string", "String payload").await;
            assert!(result.is_ok());

            let result = client
                .publish(String::from("test/owned"), vec![1, 2, 3, 4])
                .await;
            assert!(result.is_ok());

            client.disconnect().await.unwrap();
        }
        Err(e) => {
            eprintln!("Skipping test - no broker available: {e}");
        }
    }
}
