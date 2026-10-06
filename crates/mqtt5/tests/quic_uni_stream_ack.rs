#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::config::{BrokerConfig, QuicConfig, ServerDeliveryStrategy};
use mqtt5::broker::{BrokerShutdownHandle, MqttBroker};
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, PublishOptions, QoS, SubscribeOptions};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use ulid::Ulid;

async fn start_quic_broker(strategy: ServerDeliveryStrategy) -> (BrokerShutdownHandle, SocketAddr) {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs");
    let config = BrokerConfig::default()
        .with_storage(
            mqtt5::broker::config::StorageConfig::new()
                .with_backend(mqtt5::broker::config::StorageBackend::Memory),
        )
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_server_delivery_strategy(strategy)
        .with_quic(
            QuicConfig::new(cert_dir.join("server.pem"), cert_dir.join("server.key"))
                .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address")),
        );
    let mut broker = MqttBroker::with_config(config).await.expect("start broker");
    let quic_addr = broker.quic_local_addr().expect("QUIC endpoint bound");
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    tokio::spawn(async move { broker.run().await });
    ready.wait_for(|&up| up).await.expect("broker ready");
    (shutdown, quic_addr)
}

async fn payloads_seen_by_ack_callback(strategy: ServerDeliveryStrategy) -> Vec<String> {
    let (shutdown, addr) = start_quic_broker(strategy).await;
    let url = format!("quic://{addr}");

    let client_id = format!("uni-ack-sub-{}", Ulid::new());
    let options = ConnectOptions::new(&client_id)
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_receive_maximum(16);
    let subscriber = MqttClient::with_options(options.clone());
    subscriber.set_insecure_tls(true).await;
    Box::pin(subscriber.connect_with_options(&url, options))
        .await
        .expect("subscriber connects");

    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    subscriber
        .subscribe_with_ack(
            "uni/ack",
            SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |publish, token| {
                recorder
                    .lock()
                    .expect("recorder lock")
                    .push(String::from_utf8_lossy(&publish.payload).into_owned());
                token.ack();
            },
        )
        .await
        .expect("subscribe_with_ack");

    let publisher = MqttClient::new(format!("uni-ack-pub-{}", Ulid::new()));
    publisher.set_insecure_tls(true).await;
    Box::pin(publisher.connect(&url))
        .await
        .expect("publisher connects");
    for (payload, qos) in [("qos0", QoS::AtMostOnce), ("qos1", QoS::AtLeastOnce)] {
        publisher
            .publish_with_options(
                "uni/ack",
                payload.as_bytes().to_vec(),
                PublishOptions {
                    qos,
                    ..Default::default()
                },
            )
            .await
            .expect("publish");
    }

    for _ in 0..80 {
        if seen.lock().expect("seen lock").len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let mut payloads = seen.lock().expect("seen lock").clone();
    payloads.sort();
    publisher.disconnect().await.expect("publisher disconnect");
    subscriber
        .disconnect()
        .await
        .expect("subscriber disconnect");
    shutdown.shutdown();
    payloads
}

#[tokio::test]
async fn ack_callback_receives_qos0_sent_on_the_control_stream() {
    let payloads = payloads_seen_by_ack_callback(ServerDeliveryStrategy::ControlOnly).await;
    assert_eq!(payloads, ["qos0", "qos1"]);
}

#[tokio::test]
async fn ack_callback_receives_qos0_sent_on_a_unidirectional_stream() {
    let payloads = payloads_seen_by_ack_callback(ServerDeliveryStrategy::PerPublish).await;
    assert_eq!(payloads, ["qos0", "qos1"]);
}

struct LoadOutcome {
    qos0: usize,
    qos1: usize,
    qos2: usize,
    duplicates: usize,
    connected: bool,
    final_delivered: bool,
}

async fn mixed_load_seen_by_ack_callback(strategy: ServerDeliveryStrategy) -> LoadOutcome {
    let (shutdown, addr) = start_quic_broker(strategy).await;
    let url = format!("quic://{addr}");

    let client_id = format!("uni-load-sub-{}", Ulid::new());
    let options = ConnectOptions::new(&client_id)
        .with_deferred_ack(true)
        .with_clean_start(false)
        .with_session_expiry_interval(3600)
        .with_receive_maximum(16);
    let subscriber = MqttClient::with_options(options.clone());
    subscriber.set_insecure_tls(true).await;
    Box::pin(subscriber.connect_with_options(&url, options))
        .await
        .expect("subscriber connects");

    let seen = Arc::new(Mutex::new(BTreeMap::<String, usize>::new()));
    let recorder = Arc::clone(&seen);
    subscriber
        .subscribe_with_ack(
            "load/#",
            SubscribeOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
            move |publish, token| {
                *recorder
                    .lock()
                    .expect("recorder lock")
                    .entry(String::from_utf8_lossy(&publish.payload).into_owned())
                    .or_insert(0) += 1;
                token.ack();
            },
        )
        .await
        .expect("subscribe_with_ack");

    let publisher = MqttClient::new(format!("uni-load-pub-{}", Ulid::new()));
    publisher.set_insecure_tls(true).await;
    Box::pin(publisher.connect(&url))
        .await
        .expect("publisher connects");
    for i in 0..400 {
        let qos = match i % 8 {
            6 => QoS::AtLeastOnce,
            7 => QoS::ExactlyOnce,
            _ => QoS::AtMostOnce,
        };
        publisher
            .publish_with_options(
                &format!("load/{}", i % 5),
                format!("m{i}-q{}", qos as u8).into_bytes(),
                PublishOptions {
                    qos,
                    ..Default::default()
                },
            )
            .await
            .expect("publish");
    }
    for _ in 0..400 {
        if seen.lock().expect("seen lock").len() >= 400 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let connected = subscriber.is_connected().await;

    publisher
        .publish_with_options(
            "load/final",
            b"final".to_vec(),
            PublishOptions {
                qos: QoS::ExactlyOnce,
                ..Default::default()
            },
        )
        .await
        .expect("final publish");
    for _ in 0..80 {
        if seen.lock().expect("seen lock").contains_key("final") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let seen = seen.lock().expect("seen lock").clone();
    let with_qos = |suffix: &str| seen.keys().filter(|key| key.ends_with(suffix)).count();
    let outcome = LoadOutcome {
        qos0: with_qos("-q0"),
        qos1: with_qos("-q1"),
        qos2: with_qos("-q2"),
        duplicates: seen.values().filter(|&&count| count > 1).count(),
        connected,
        final_delivered: seen.contains_key("final"),
    };
    publisher.disconnect().await.expect("publisher disconnect");
    subscriber
        .disconnect()
        .await
        .expect("subscriber disconnect");
    shutdown.shutdown();
    outcome
}

fn assert_every_message_delivered_once(outcome: &LoadOutcome) {
    assert_eq!(
        (outcome.qos0, outcome.qos1, outcome.qos2, outcome.duplicates),
        (300, 50, 50, 0)
    );
    assert!(outcome.connected);
    assert!(outcome.final_delivered);
}

#[tokio::test]
async fn ack_callback_receives_mixed_load_sent_on_the_control_stream() {
    let outcome = mixed_load_seen_by_ack_callback(ServerDeliveryStrategy::ControlOnly).await;
    assert_every_message_delivered_once(&outcome);
}

#[tokio::test]
async fn ack_callback_receives_mixed_load_sent_on_unidirectional_streams() {
    let outcome = mixed_load_seen_by_ack_callback(ServerDeliveryStrategy::PerPublish).await;
    assert_every_message_delivered_once(&outcome);
}
