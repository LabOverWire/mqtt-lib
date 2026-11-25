#![cfg(feature = "broker")]
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

async fn broker() -> SocketAddr {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: true,
            ..Default::default()
        });
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let addr = broker.local_addr().unwrap();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&up| up).await.unwrap();
    addr
}

fn persistent(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_clean_start(false)
        .with_session_expiry_interval(300)
        .with_automatic_reconnect(false)
}

async fn create_session_for(url: &str, id: &str) {
    let other = MqttClient::new(id);
    other
        .connect_with_options(url, persistent(id))
        .await
        .unwrap();
    other.disconnect().await.unwrap();
}

async fn subscription_survives_switch(new_id_has_session: bool) -> (bool, Vec<String>) {
    let addr = broker().await;
    let url = format!("mqtt://{addr}");
    if new_id_has_session {
        create_session_for(&url, "sb").await;
    }
    let client = MqttClient::new("sa");
    client
        .connect_with_options(&url, persistent("sa"))
        .await
        .unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    client
        .subscribe_with_options(
            "t/a",
            mqtt5::SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |message| {
                sink.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&message.payload).into_owned());
            },
        )
        .await
        .unwrap();
    client.disconnect().await.unwrap();
    let result = client
        .connect_with_options(&url, persistent("sb"))
        .await
        .unwrap();
    assert_eq!(client.client_id().await, "sb");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let publisher = MqttClient::new("pub346b");
    publisher.connect(&url).await.unwrap();
    publisher
        .publish_qos1("t/a", b"after-switch".to_vec())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    publisher.disconnect().await.unwrap();
    let _ = client.disconnect().await;
    let payloads = seen.lock().unwrap().clone();
    (result.session_present, payloads)
}

#[tokio::test]
async fn a_subscription_keeps_receiving_after_switching_to_an_id_with_a_session() {
    let (session_present, payloads) = subscription_survives_switch(true).await;
    assert!(session_present);
    assert!(
        payloads.iter().any(|p| p == "after-switch"),
        "received: {payloads:?}"
    );
}

#[tokio::test]
async fn control_a_subscription_keeps_receiving_after_switching_to_an_id_without_a_session() {
    let (session_present, payloads) = subscription_survives_switch(false).await;
    assert!(!session_present);
    assert!(
        payloads.iter().any(|p| p == "after-switch"),
        "received: {payloads:?}"
    );
}

#[tokio::test]
async fn a_plain_connect_after_a_cancelled_connect_resumes_the_assigned_session() {
    let addr = broker().await;
    let url = format!("mqtt://{addr}");
    let client = MqttClient::new("");
    let first = client
        .connect_with_options(&url, persistent(""))
        .await
        .unwrap();
    assert!(!first.session_present);
    let assigned = client.client_id().await;
    assert!(!assigned.is_empty());
    client.disconnect().await.unwrap();

    let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent_url = format!("mqtt://{}", silent.local_addr().unwrap());
    let cancelled = tokio::time::timeout(
        Duration::from_millis(300),
        client.connect_with_options(&silent_url, persistent("other")),
    )
    .await;
    assert!(cancelled.is_err());
    drop(silent);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(client.client_id().await, assigned);

    let resumed = Arc::new(Mutex::new(Vec::new()));
    let resumed_sink = Arc::clone(&resumed);
    client
        .on_connection_event(move |event| {
            if let mqtt5::ConnectionEvent::Connected {
                session_present, ..
            } = event
            {
                resumed_sink.lock().unwrap().push(session_present);
            }
        })
        .await
        .unwrap();
    client.connect(&url).await.unwrap();
    assert_eq!(client.client_id().await, assigned);
    assert_eq!(resumed.lock().unwrap().clone(), vec![true]);
    let publisher = MqttClient::new("pub346b-c");
    publisher.connect(&url).await.unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    client
        .subscribe("t/c", move |message| {
            sink.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&message.payload).into_owned());
        })
        .await
        .unwrap();
    publisher
        .publish_qos1("t/c", b"resumed".to_vec())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    publisher.disconnect().await.unwrap();
    client.disconnect().await.unwrap();
    assert_eq!(seen.lock().unwrap().clone(), vec!["resumed".to_string()]);
}
