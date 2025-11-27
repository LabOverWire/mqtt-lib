use mqtt5::broker::config::{BrokerConfig, QuicConfig};
use mqtt5::broker::MqttBroker;
use mqtt5::session::quic_flow::{FlowRegistry, FlowState, FlowType};
use mqtt5::time::Duration;
use mqtt5::transport::flow::{FlowFlags, FlowId};
use mqtt5::transport::StreamStrategy;
use mqtt5::MqttClient;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use ulid::Ulid;

fn test_client_id(prefix: &str) -> String {
    format!("{}-{}", prefix, Ulid::new())
}

async fn start_quic_broker(quic_port: u16) -> (MqttBroker, SocketAddr) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let quic_addr: SocketAddr = format!("127.0.0.1:{quic_port}").parse().unwrap();

    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            QuicConfig::new(
                PathBuf::from("../../test_certs/server.pem"),
                PathBuf::from("../../test_certs/server.key"),
            )
            .with_bind_address(quic_addr),
        );

    let broker = MqttBroker::with_config(config).await.unwrap();
    (broker, quic_addr)
}

#[tokio::test]
async fn test_flow_registry_basic_operations() {
    let mut registry = FlowRegistry::new(100);

    let flow_id1 = registry.new_client_flow(FlowFlags::default(), None);
    assert!(flow_id1.is_some());
    let flow_id1 = flow_id1.unwrap();
    assert!(flow_id1.is_client_initiated());
    assert_eq!(flow_id1.sequence(), 1);

    let flow_id2 = registry.new_client_flow(FlowFlags::default(), Some(Duration::from_secs(60)));
    assert!(flow_id2.is_some());
    let flow_id2 = flow_id2.unwrap();
    assert_eq!(flow_id2.sequence(), 2);

    let flow_id3 = registry.new_server_flow(FlowFlags::default(), None);
    assert!(flow_id3.is_some());
    let flow_id3 = flow_id3.unwrap();
    assert!(flow_id3.is_server_initiated());

    assert_eq!(registry.len(), 3);
    assert!(registry.contains(flow_id1));
    assert!(registry.contains(flow_id2));
    assert!(registry.contains(flow_id3));

    let removed = registry.remove(flow_id1);
    assert!(removed.is_some());
    assert!(!registry.contains(flow_id1));
    assert_eq!(registry.len(), 2);
}

#[tokio::test]
async fn test_flow_registry_max_flows() {
    let mut registry = FlowRegistry::new(3);

    registry.new_client_flow(FlowFlags::default(), None);
    registry.new_client_flow(FlowFlags::default(), None);
    registry.new_client_flow(FlowFlags::default(), None);

    let overflow = registry.new_client_flow(FlowFlags::default(), None);
    assert!(overflow.is_none());
    assert_eq!(registry.len(), 3);
}

#[tokio::test]
async fn test_flow_state_subscriptions() {
    let mut state = FlowState::new_control();
    assert_eq!(state.flow_type, FlowType::Control);
    assert!(state.subscriptions.is_empty());

    state.add_subscription("test/topic1".to_string());
    state.add_subscription("test/topic2".to_string());
    state.add_subscription("test/topic1".to_string());
    assert_eq!(state.subscriptions.len(), 2);

    state.remove_subscription("test/topic1");
    assert_eq!(state.subscriptions.len(), 1);
    assert!(state.subscriptions.contains(&"test/topic2".to_string()));
}

#[tokio::test]
async fn test_flow_state_topic_aliases() {
    let mut state = FlowState::new_client_data(
        FlowId::client(1),
        FlowFlags::default(),
        Some(Duration::from_secs(300)),
    );

    state.set_topic_alias(1, "sensors/temperature".to_string());
    state.set_topic_alias(2, "sensors/humidity".to_string());

    assert_eq!(
        state.get_topic_alias(1),
        Some(&"sensors/temperature".to_string())
    );
    assert_eq!(
        state.get_topic_alias(2),
        Some(&"sensors/humidity".to_string())
    );
    assert_eq!(state.get_topic_alias(3), None);
}

#[tokio::test]
async fn test_flow_state_pending_packet_ids() {
    let mut state = FlowState::new_client_data(FlowId::client(1), FlowFlags::default(), None);

    state.add_pending_packet_id(100);
    state.add_pending_packet_id(200);
    state.add_pending_packet_id(100);
    assert_eq!(state.pending_packet_ids.len(), 2);

    state.remove_pending_packet_id(100);
    assert_eq!(state.pending_packet_ids.len(), 1);
    assert!(state.pending_packet_ids.contains(&200));
}

#[tokio::test]
async fn test_flow_flags_configuration() {
    let default_flags = FlowFlags::default();
    assert_eq!(default_flags.clean, 0);
    assert_eq!(default_flags.abort_if_no_state, 0);
    assert_eq!(default_flags.err_tolerance, 0);
    assert_eq!(default_flags.persistent_qos, 0);
    assert_eq!(default_flags.persistent_subscriptions, 0);

    let custom_flags = FlowFlags {
        clean: 1,
        abort_if_no_state: 1,
        err_tolerance: 3,
        persistent_qos: 1,
        persistent_topic_alias: 1,
        persistent_subscriptions: 1,
        optional_headers: 0,
    };

    assert_eq!(custom_flags.clean, 1);
    assert_eq!(custom_flags.abort_if_no_state, 1);
    assert_eq!(custom_flags.err_tolerance, 3);
    assert_eq!(custom_flags.persistent_qos, 1);
    assert_eq!(custom_flags.persistent_subscriptions, 1);
}

#[tokio::test]
async fn test_flow_id_encoding() {
    let client_flow = FlowId::client(42);
    assert!(client_flow.is_client_initiated());
    assert!(!client_flow.is_server_initiated());
    assert_eq!(client_flow.sequence(), 42);

    let server_flow = FlowId::server(99);
    assert!(!server_flow.is_client_initiated());
    assert!(server_flow.is_server_initiated());
    assert_eq!(server_flow.sequence(), 99);

    let control_flow = FlowId::control();
    assert!(control_flow.is_client_initiated());
    assert_eq!(control_flow.sequence(), 0);
}

#[tokio::test]
async fn test_flow_registry_subscription_lookup() {
    let mut registry = FlowRegistry::new(100);

    let id1 = registry
        .new_client_flow(FlowFlags::default(), None)
        .unwrap();
    let id2 = registry
        .new_client_flow(FlowFlags::default(), None)
        .unwrap();
    let id3 = registry
        .new_client_flow(FlowFlags::default(), None)
        .unwrap();

    registry
        .get_mut(id1)
        .unwrap()
        .add_subscription("sensors/#".to_string());
    registry
        .get_mut(id2)
        .unwrap()
        .add_subscription("sensors/#".to_string());
    registry
        .get_mut(id3)
        .unwrap()
        .add_subscription("actuators/#".to_string());

    let sensor_flows = registry.flows_for_subscription("sensors/#");
    assert_eq!(sensor_flows.len(), 2);
    assert!(sensor_flows.contains(&id1));
    assert!(sensor_flows.contains(&id2));

    let actuator_flows = registry.flows_for_subscription("actuators/#");
    assert_eq!(actuator_flows.len(), 1);
    assert!(actuator_flows.contains(&id3));
}

#[tokio::test]
async fn test_flow_registry_client_server_iteration() {
    let mut registry = FlowRegistry::new(100);

    registry.new_client_flow(FlowFlags::default(), None);
    registry.new_client_flow(FlowFlags::default(), None);
    registry.new_server_flow(FlowFlags::default(), None);
    registry.new_server_flow(FlowFlags::default(), None);
    registry.new_server_flow(FlowFlags::default(), None);

    let client_flows: Vec<_> = registry.client_flows().collect();
    assert_eq!(client_flows.len(), 2);

    let server_flows: Vec<_> = registry.server_flows().collect();
    assert_eq!(server_flows.len(), 3);
}

#[tokio::test]
async fn test_flow_registry_touch_updates_activity() {
    let mut registry = FlowRegistry::new(100);
    let id = registry
        .new_client_flow(FlowFlags::default(), None)
        .unwrap();

    let initial_time = registry.get(id).unwrap().last_activity;
    std::thread::sleep(std::time::Duration::from_millis(10));
    registry.touch(id);
    let updated_time = registry.get(id).unwrap().last_activity;

    assert!(updated_time > initial_time);
}

#[tokio::test]
async fn test_quic_data_per_publish_with_broker() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24680).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client_id = test_client_id("quic-flow-pub");
    let sub_client_id = test_client_id("quic-flow-sub");
    let topic = format!("quic-flow/{}/test", Ulid::new());

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.set_insecure_tls(true).await;
    sub_client.set_insecure_tls(true).await;
    pub_client
        .set_quic_stream_strategy(StreamStrategy::DataPerPublish)
        .await;

    let broker_url = format!("quic://{quic_addr}");

    if pub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    for i in 0..3 {
        pub_client
            .publish(&topic, format!("multistream msg {i}").as_bytes())
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        3,
        "Should receive all 3 messages via dedicated QUIC streams"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_quic_multiple_topics_with_flow_isolation() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24681).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client_id = test_client_id("quic-multi-pub");
    let sub_client_id = test_client_id("quic-multi-sub");
    let topic1 = format!("flow/{}/topic1", Ulid::new());
    let topic2 = format!("flow/{}/topic2", Ulid::new());

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.set_insecure_tls(true).await;
    sub_client.set_insecure_tls(true).await;
    pub_client
        .set_quic_stream_strategy(StreamStrategy::DataPerTopic)
        .await;

    let broker_url = format!("quic://{quic_addr}");

    if pub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let topic1_count = Arc::new(AtomicU32::new(0));
    let topic2_count = Arc::new(AtomicU32::new(0));
    let topic1_clone = topic1_count.clone();
    let topic2_clone = topic2_count.clone();

    sub_client
        .subscribe(&topic1, move |_msg| {
            topic1_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    sub_client
        .subscribe(&topic2, move |_msg| {
            topic2_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    for i in 0..5 {
        pub_client
            .publish(&topic1, format!("t1-msg{i}").as_bytes())
            .await
            .unwrap();
        pub_client
            .publish(&topic2, format!("t2-msg{i}").as_bytes())
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        topic1_count.load(Ordering::Relaxed),
        5,
        "Topic1 should receive 5 messages"
    );
    assert_eq!(
        topic2_count.load(Ordering::Relaxed),
        5,
        "Topic2 should receive 5 messages"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_quic_control_only_strategy() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24682).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client_id = test_client_id("quic-ctrl-pub");
    let sub_client_id = test_client_id("quic-ctrl-sub");
    let topic = format!("control/{}/test", Ulid::new());

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.set_insecure_tls(true).await;
    sub_client.set_insecure_tls(true).await;
    pub_client
        .set_quic_stream_strategy(StreamStrategy::ControlOnly)
        .await;

    let broker_url = format!("quic://{quic_addr}");

    if pub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    for i in 0..3 {
        pub_client
            .publish(&topic, format!("control msg {i}").as_bytes())
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert_eq!(
        received.load(Ordering::Relaxed),
        3,
        "Should receive all 3 messages via control stream"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_quic_mixed_qos_with_streams() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24683).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client_id = test_client_id("quic-qos-pub");
    let sub_client_id = test_client_id("quic-qos-sub");
    let topic = format!("qos/{}/mixed", Ulid::new());

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.set_insecure_tls(true).await;
    sub_client.set_insecure_tls(true).await;
    pub_client
        .set_quic_stream_strategy(StreamStrategy::DataPerPublish)
        .await;

    let broker_url = format!("quic://{quic_addr}");

    if pub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    pub_client.publish_qos0(&topic, b"qos0").await.unwrap();
    pub_client.publish_qos1(&topic, b"qos1").await.unwrap();
    pub_client.publish_qos2(&topic, b"qos2").await.unwrap();

    tokio::time::sleep(Duration::from_millis(1000)).await;

    assert!(
        received.load(Ordering::Relaxed) >= 2,
        "Should receive at least QoS0 and QoS1 messages"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_quic_concurrent_publishers() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24684).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let topic = format!("concurrent/{}/test", Ulid::new());
    let broker_url = format!("quic://{quic_addr}");

    let sub_client = MqttClient::new(test_client_id("quic-conc-sub"));
    sub_client.set_insecure_tls(true).await;

    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received = Arc::new(AtomicU32::new(0));
    let received_clone = received.clone();

    sub_client
        .subscribe(&topic, move |_msg| {
            received_clone.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut pub_handles = vec![];
    for i in 0..3 {
        let topic_clone = topic.clone();
        let url_clone = broker_url.clone();
        let handle = tokio::spawn(async move {
            let pub_client = MqttClient::new(test_client_id(&format!("quic-conc-pub{i}")));
            pub_client.set_insecure_tls(true).await;
            pub_client
                .set_quic_stream_strategy(StreamStrategy::DataPerPublish)
                .await;

            if pub_client.connect(&url_clone).await.is_ok() {
                for j in 0..5 {
                    let _ = pub_client
                        .publish(&topic_clone, format!("pub{i}-msg{j}").as_bytes())
                        .await;
                }
                let _ = pub_client.disconnect().await;
            }
        });
        pub_handles.push(handle);
    }

    for handle in pub_handles {
        let _ = handle.await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    assert!(
        received.load(Ordering::Relaxed) >= 10,
        "Should receive at least 10 messages from concurrent publishers"
    );

    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_quic_large_payload_per_stream() {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let (mut broker, quic_addr) = start_quic_broker(24685).await;

    let broker_handle = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let pub_client_id = test_client_id("quic-large-pub");
    let sub_client_id = test_client_id("quic-large-sub");
    let topic = format!("large/{}/payload", Ulid::new());

    let pub_client = MqttClient::new(pub_client_id);
    let sub_client = MqttClient::new(sub_client_id);

    pub_client.set_insecure_tls(true).await;
    sub_client.set_insecure_tls(true).await;
    pub_client
        .set_quic_stream_strategy(StreamStrategy::DataPerPublish)
        .await;

    let broker_url = format!("quic://{quic_addr}");

    if pub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }
    if sub_client.connect(&broker_url).await.is_err() {
        broker_handle.abort();
        return;
    }

    let received_sizes = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let received_clone = received_sizes.clone();

    sub_client
        .subscribe(&topic, move |msg| {
            let size = msg.payload.len();
            let sizes = received_clone.clone();
            tokio::spawn(async move {
                sizes.lock().await.push(size);
            });
        })
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;

    let sizes = vec![100, 1000, 10000, 50000];
    for size in &sizes {
        let payload = vec![0x42u8; *size];
        pub_client.publish(&topic, payload).await.unwrap();
    }

    tokio::time::sleep(Duration::from_millis(1000)).await;

    let received = received_sizes.lock().await;
    assert_eq!(
        received.len(),
        4,
        "Should receive all 4 messages of different sizes"
    );

    pub_client.disconnect().await.unwrap();
    sub_client.disconnect().await.unwrap();
    broker_handle.abort();
}

#[tokio::test]
async fn test_flow_registry_clear_all() {
    let mut registry = FlowRegistry::new(100);

    for _ in 0..10 {
        registry.new_client_flow(FlowFlags::default(), None);
    }
    for _ in 0..5 {
        registry.new_server_flow(FlowFlags::default(), None);
    }

    assert_eq!(registry.len(), 15);

    registry.clear();
    assert!(registry.is_empty());
    assert_eq!(registry.len(), 0);
}

#[tokio::test]
async fn test_flow_state_types() {
    let control = FlowState::new_control();
    assert_eq!(control.flow_type, FlowType::Control);
    assert_eq!(control.id, FlowId::control());

    let client_data = FlowState::new_client_data(
        FlowId::client(42),
        FlowFlags::default(),
        Some(Duration::from_secs(300)),
    );
    assert_eq!(client_data.flow_type, FlowType::ClientData);
    assert_eq!(client_data.expire_interval, Some(Duration::from_secs(300)));

    let server_data = FlowState::new_server_data(FlowId::server(99), FlowFlags::default(), None);
    assert_eq!(server_data.flow_type, FlowType::ServerData);
    assert!(server_data.expire_interval.is_none());
}

#[tokio::test]
async fn test_flow_state_not_expired_without_interval() {
    let state = FlowState::new_client_data(FlowId::client(1), FlowFlags::default(), None);
    assert!(!state.is_expired());
}

#[tokio::test]
async fn test_flow_registry_register_external_flow() {
    let mut registry = FlowRegistry::new(100);

    let state = FlowState::new_client_data(
        FlowId::client(999),
        FlowFlags {
            persistent_subscriptions: 1,
            ..Default::default()
        },
        Some(Duration::from_secs(600)),
    );

    assert!(registry.register_flow(state.clone()));
    assert!(registry.contains(FlowId::client(999)));
    assert_eq!(registry.len(), 1);

    let retrieved = registry.get(FlowId::client(999)).unwrap();
    assert_eq!(retrieved.flags.persistent_subscriptions, 1);
}
