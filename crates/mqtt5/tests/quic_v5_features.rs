#![cfg(all(feature = "broker", feature = "transport-quic"))]

use mqtt5::broker::config::{
    BrokerConfig, QuicConfig, ServerDeliveryStrategy, StorageBackend, StorageConfig,
};
use mqtt5::broker::{BrokerShutdownHandle, MqttBroker};
use mqtt5::time::Duration;
use mqtt5::transport::StreamStrategy;
use mqtt5::{
    ConnectOptions, Delivery, Message, MqttClient, PublishOptions, PublishProperties,
    PublishResult, QoS, SubscribeOptions, WillMessage,
};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Display;
use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{Arc, Mutex};
use ulid::Ulid;

#[derive(Debug, Clone, Copy)]
enum Link {
    Tcp,
    Quic {
        client: StreamStrategy,
        broker: ServerDeliveryStrategy,
    },
}

#[derive(Debug, Clone, Copy)]
struct Mode {
    name: &'static str,
    link: Link,
}

const MODES: [Mode; 6] = [
    Mode {
        name: "tcp",
        link: Link::Tcp,
    },
    Mode {
        name: "quic control-only",
        link: Link::Quic {
            client: StreamStrategy::ControlOnly,
            broker: ServerDeliveryStrategy::ControlOnly,
        },
    },
    Mode {
        name: "quic per-topic",
        link: Link::Quic {
            client: StreamStrategy::DataPerTopic,
            broker: ServerDeliveryStrategy::PerTopic,
        },
    },
    Mode {
        name: "quic per-publish",
        link: Link::Quic {
            client: StreamStrategy::DataPerPublish,
            broker: ServerDeliveryStrategy::PerPublish,
        },
    },
    Mode {
        name: "quic mixed client per-publish / broker control-only",
        link: Link::Quic {
            client: StreamStrategy::DataPerPublish,
            broker: ServerDeliveryStrategy::ControlOnly,
        },
    },
    Mode {
        name: "quic mixed client control-only / broker per-publish",
        link: Link::Quic {
            client: StreamStrategy::ControlOnly,
            broker: ServerDeliveryStrategy::PerPublish,
        },
    },
];

#[derive(Clone)]
struct Env {
    url: String,
    link: Link,
    base: String,
}

impl Env {
    async fn client(
        &self,
        prefix: &str,
        configure: impl FnOnce(ConnectOptions) -> ConnectOptions,
    ) -> Result<MqttClient, String> {
        let options = configure(ConnectOptions::new(format!("{prefix}-{}", Ulid::new())));
        let client = MqttClient::with_options(options.clone());
        if let Link::Quic {
            client: strategy, ..
        } = self.link
        {
            client.set_insecure_tls(true).await;
            client.set_quic_stream_strategy(strategy).await;
        }
        Box::pin(client.connect_with_options(&self.url, options))
            .await
            .map_err(fail(&format!("{prefix} connect")))?;
        Ok(client)
    }

    async fn plain(&self, prefix: &str) -> Result<MqttClient, String> {
        self.client(prefix, |options| options).await
    }

    fn topic(&self, suffix: &str) -> String {
        format!("{}/{suffix}", self.base)
    }
}

fn fail<E: Display>(context: &str) -> impl FnOnce(E) -> String + '_ {
    move |e| format!("{context}: {e}")
}

fn ensure(condition: bool, message: impl FnOnce() -> String) -> Result<(), String> {
    if condition {
        Ok(())
    } else {
        Err(message())
    }
}

async fn start_broker(link: Link) -> (BrokerShutdownHandle, String) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test_certs");
    let quic_bind: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let delivery = match link {
        Link::Tcp => ServerDeliveryStrategy::default(),
        Link::Quic { broker, .. } => broker,
    };
    let config = BrokerConfig::default()
        .with_storage(StorageConfig::new().with_backend(StorageBackend::Memory))
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_quic(
            QuicConfig::new(cert_dir.join("server.pem"), cert_dir.join("server.key"))
                .with_bind_address(quic_bind),
        )
        .with_server_delivery_strategy(delivery);
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let url = match link {
        Link::Tcp => format!("mqtt://{}", broker.local_addr().unwrap()),
        Link::Quic { .. } => format!("quic://{}", broker.quic_local_addr().unwrap()),
    };
    let mut ready = broker.ready_receiver();
    let shutdown = broker.shutdown_handle();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    let _ = ready.wait_for(|&v| v).await;
    (shutdown, url)
}

async fn run_matrix<F, Fut>(feature: &str, check: F)
where
    F: Fn(Env) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let mut failures = Vec::new();
    for mode in MODES {
        let (shutdown, url) = start_broker(mode.link).await;
        let env = Env {
            url,
            link: mode.link,
            base: format!("v5/{}", Ulid::new()),
        };
        let outcome = check(env).await;
        shutdown.shutdown();
        match outcome {
            Ok(()) => println!("MATRIX | {feature} | {} | PASS", mode.name),
            Err(reason) => {
                println!("MATRIX | {feature} | {} | FAIL | {reason}", mode.name);
                failures.push(format!("{}: {reason}", mode.name));
            }
        }
    }
    assert!(failures.is_empty(), "{feature} failed: {failures:#?}");
}

type Inbox = Arc<Mutex<Vec<Message>>>;
type Arrivals = Arc<Mutex<Vec<(u32, Option<u64>)>>>;

async fn subscribe_into(
    client: &MqttClient,
    filter: &str,
    options: SubscribeOptions,
) -> Result<Inbox, String> {
    let inbox: Inbox = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&inbox);
    client
        .subscribe_with_options(filter, options, move |message| {
            sink.lock().unwrap().push(message);
        })
        .await
        .map_err(fail(&format!("subscribe {filter}")))?;
    Ok(inbox)
}

fn sub_qos(qos: QoS) -> SubscribeOptions {
    SubscribeOptions {
        qos,
        ..Default::default()
    }
}

fn pub_qos(qos: QoS) -> PublishOptions {
    PublishOptions {
        qos,
        ..Default::default()
    }
}

async fn wait_for_count(inboxes: &[&Inbox], expected: usize) -> usize {
    let total = || {
        inboxes
            .iter()
            .map(|i| i.lock().unwrap().len())
            .sum::<usize>()
    };
    for _ in 0..400 {
        if total() >= expected {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    total()
}

fn payload_index(message: &Message) -> Option<u32> {
    std::str::from_utf8(&message.payload).ok()?.parse().ok()
}

async fn check_shared_subscription(env: Env) -> Result<(), String> {
    let topic = env.topic("jobs");
    let filter = format!("$share/workers/{topic}");
    let member_a = env.plain("share-a").await?;
    let member_b = env.plain("share-b").await?;
    let inbox_a = subscribe_into(&member_a, &filter, sub_qos(QoS::AtLeastOnce)).await?;
    let inbox_b = subscribe_into(&member_b, &filter, sub_qos(QoS::AtLeastOnce)).await?;
    let publisher = env.plain("share-pub").await?;
    let count = 40u32;
    for i in 0..count {
        publisher
            .publish_with_options(&topic, i.to_string(), pub_qos(QoS::AtLeastOnce))
            .await
            .map_err(fail("shared publish"))?;
    }
    let total = wait_for_count(&[&inbox_a, &inbox_b], count as usize).await;
    let a = inbox_a.lock().unwrap().clone();
    let b = inbox_b.lock().unwrap().clone();
    let indices: Vec<u32> = a.iter().chain(b.iter()).filter_map(payload_index).collect();
    let unique: BTreeSet<u32> = indices.iter().copied().collect();
    ensure(total == count as usize, || {
        format!("expected {count} deliveries across the group, got {total}")
    })?;
    ensure(unique == (0..count).collect(), || {
        format!("not every message reached the group: {unique:?}")
    })?;
    ensure(indices.len() == unique.len(), || {
        format!("duplicates across members: {indices:?}")
    })?;
    ensure(!a.is_empty() && !b.is_empty(), || {
        format!("not distributed: a={} b={}", a.len(), b.len())
    })?;
    ensure(
        a.iter().chain(b.iter()).all(|m| m.qos == QoS::AtLeastOnce),
        || "a shared delivery was not QoS 1".to_string(),
    )?;
    println!(
        "DETAIL | shared | {} | member_a={} member_b={}",
        env.url,
        a.len(),
        b.len()
    );
    publisher.disconnect().await.ok();
    member_a.disconnect().await.ok();
    member_b.disconnect().await.ok();
    Ok(())
}

async fn check_request_response(env: Env) -> Result<(), String> {
    let request_topic = env.topic("rpc/req");
    let response_topic = env.topic(&format!("rpc/resp/{}", Ulid::new()));
    let responder = env.plain("rpc-server").await?;
    let replier = responder.clone();
    responder
        .subscribe_with_options(&request_topic, sub_qos(QoS::AtLeastOnce), move |request| {
            let replier = replier.clone();
            tokio::spawn(async move {
                let Some(reply_to) = request.properties.response_topic.clone() else {
                    return;
                };
                let options = PublishOptions {
                    qos: QoS::AtLeastOnce,
                    properties: PublishProperties {
                        correlation_data: request.properties.correlation_data.clone(),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                let mut body = b"reply:".to_vec();
                body.extend_from_slice(&request.payload);
                let _ = replier.publish_with_options(reply_to, body, options).await;
            });
        })
        .await
        .map_err(fail("responder subscribe"))?;
    let requester = env.plain("rpc-client").await?;
    let responses = subscribe_into(&requester, &response_topic, sub_qos(QoS::AtLeastOnce)).await?;
    let count = 10u32;
    for i in 0..count {
        let options = PublishOptions {
            qos: QoS::AtLeastOnce,
            properties: PublishProperties {
                response_topic: Some(response_topic.clone()),
                correlation_data: Some(format!("corr-{i}").into_bytes()),
                ..Default::default()
            },
            ..Default::default()
        };
        requester
            .publish_with_options(&request_topic, i.to_string(), options)
            .await
            .map_err(fail("request publish"))?;
    }
    let total = wait_for_count(&[&responses], count as usize).await;
    ensure(total == count as usize, || {
        format!("expected {count} responses, got {total}")
    })?;
    let matched: HashMap<Vec<u8>, Vec<u8>> = responses
        .lock()
        .unwrap()
        .iter()
        .filter_map(|m| {
            m.properties
                .correlation_data
                .clone()
                .map(|c| (c, m.payload.clone()))
        })
        .collect();
    for i in 0..count {
        let correlation = format!("corr-{i}").into_bytes();
        let expected = format!("reply:{i}").into_bytes();
        ensure(matched.get(&correlation) == Some(&expected), || {
            format!("correlation corr-{i} not matched to its reply: {matched:?}")
        })?;
    }
    requester.disconnect().await.ok();
    responder.disconnect().await.ok();
    Ok(())
}

async fn check_retained(env: Env) -> Result<(), String> {
    let topic = env.topic("state");
    let publisher = env.plain("retain-pub").await?;
    publisher
        .publish_with_options(
            &topic,
            "on",
            PublishOptions {
                qos: QoS::AtLeastOnce,
                retain: true,
                ..Default::default()
            },
        )
        .await
        .map_err(fail("retained publish"))?;
    let late = env.plain("retain-late").await?;
    let inbox = subscribe_into(&late, &topic, sub_qos(QoS::AtLeastOnce)).await?;
    let total = wait_for_count(&[&inbox], 1).await;
    let messages = inbox.lock().unwrap().clone();
    ensure(total == 1, || {
        format!("expected exactly one retained message, got {total}")
    })?;
    ensure(messages[0].retain && messages[0].payload == b"on", || {
        format!(
            "retained message wrong: retain={} payload={:?}",
            messages[0].retain, messages[0].payload
        )
    })?;
    late.disconnect().await.ok();
    publisher.disconnect().await.ok();
    Ok(())
}

async fn check_will(env: Env) -> Result<(), String> {
    let will_topic = env.topic("will");
    let watcher = env.plain("will-watch").await?;
    let inbox = subscribe_into(&watcher, &will_topic, sub_qos(QoS::AtLeastOnce)).await?;
    let will_options = |payload: &'static str| {
        let will = WillMessage::new(will_topic.clone(), payload).with_qos(QoS::AtLeastOnce);
        move |options: ConnectOptions| options.with_automatic_reconnect(false).with_will(will)
    };
    let polite = env.client("will-polite", will_options("polite")).await?;
    polite
        .disconnect()
        .await
        .map_err(fail("normal disconnect"))?;
    let abrupt = env.client("will-abrupt", will_options("abrupt")).await?;
    abrupt
        .disconnect_abnormally()
        .await
        .map_err(fail("abnormal disconnect"))?;
    let total = wait_for_count(&[&inbox], 1).await;
    let payloads: Vec<Vec<u8>> = inbox
        .lock()
        .unwrap()
        .iter()
        .map(|m| m.payload.clone())
        .collect();
    ensure(total == 1 && payloads[0] == b"abrupt", || {
        format!("expected only the abrupt client's will, got {payloads:?}")
    })?;
    watcher.disconnect().await.ok();
    Ok(())
}

async fn check_topic_alias(env: Env) -> Result<(), String> {
    let topic = env.topic("alias");
    let subscriber = env.plain("alias-sub").await?;
    let inbox = subscribe_into(&subscriber, &topic, sub_qos(QoS::AtLeastOnce)).await?;
    let publisher = env.plain("alias-pub").await?;
    let aliased = |qos| PublishOptions {
        qos,
        properties: PublishProperties {
            topic_alias: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, "0", aliased(QoS::AtLeastOnce))
        .await
        .map_err(fail("alias-establishing publish"))?;
    let count = 10u32;
    for i in 1..count {
        publisher
            .publish_with_options("", i.to_string(), aliased(QoS::AtLeastOnce))
            .await
            .map_err(fail("alias-only publish"))?;
    }
    let total = wait_for_count(&[&inbox], count as usize).await;
    let messages = inbox.lock().unwrap().clone();
    ensure(total == count as usize, || {
        format!("expected {count} aliased deliveries, got {total}")
    })?;
    ensure(messages.iter().all(|m| m.topic == topic), || {
        "a delivery lost its resolved topic name".to_string()
    })?;
    ensure(publisher.is_connected().await, || {
        "publisher was disconnected".to_string()
    })?;
    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    Ok(())
}

async fn check_topic_alias_qos0_burst(env: Env) -> Result<(), String> {
    let topic = env.topic("alias-burst");
    let subscriber = env.plain("alias-burst-sub").await?;
    let inbox = subscribe_into(&subscriber, &topic, sub_qos(QoS::AtMostOnce)).await?;
    let publisher = env.plain("alias-burst-pub").await?;
    let aliased = PublishOptions {
        qos: QoS::AtMostOnce,
        properties: PublishProperties {
            topic_alias: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, "0", aliased.clone())
        .await
        .map_err(fail("alias-establishing publish"))?;
    let count = 200u32;
    for i in 1..count {
        publisher
            .publish_with_options("", i.to_string(), aliased.clone())
            .await
            .map_err(fail(&format!("alias-only publish {i}")))?;
    }
    let total = wait_for_count(&[&inbox], count as usize).await;
    let connected = publisher.is_connected().await;
    ensure(connected, || {
        format!("publisher disconnected after the alias burst; subscriber saw {total}/{count}")
    })?;
    ensure(total == count as usize, || {
        format!("expected {count} aliased QoS 0 deliveries, got {total}")
    })?;
    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    Ok(())
}

async fn check_topic_alias_after_large_publish(env: Env, size: usize) -> Result<(), String> {
    let topic = env.topic("alias-large");
    let subscriber = env.plain("alias-large-sub").await?;
    let inbox = subscribe_into(&subscriber, &topic, sub_qos(QoS::AtMostOnce)).await?;
    let publisher = env.plain("alias-large-pub").await?;
    let rounds = 20u16;
    let large = vec![b'L'; size];
    for alias in 1..=rounds {
        let aliased = PublishOptions {
            qos: QoS::AtMostOnce,
            properties: PublishProperties {
                topic_alias: Some(alias),
                ..Default::default()
            },
            ..Default::default()
        };
        publisher
            .publish_with_options(&topic, large.clone(), aliased.clone())
            .await
            .map_err(fail(&format!("large alias-establishing publish {alias}")))?;
        publisher
            .publish_with_options("", "small", aliased)
            .await
            .map_err(fail(&format!("alias-only publish {alias}")))?;
    }
    let expected = usize::from(rounds) * 2;
    let total = wait_for_count(&[&inbox], expected).await;
    let connected = publisher.is_connected().await;
    ensure(connected, || {
        format!("publisher disconnected; subscriber saw {total}/{expected}")
    })?;
    ensure(total == expected, || {
        format!("expected {expected} deliveries, got {total}")
    })?;
    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    Ok(())
}

async fn check_topic_alias_remap(env: Env, size: usize) -> Result<(), String> {
    let first = env.topic("remap/first");
    let second = env.topic("remap/second");
    let subscriber = env.plain("remap-sub").await?;
    let first_inbox = subscribe_into(&subscriber, &first, sub_qos(QoS::AtMostOnce)).await?;
    let second_inbox = subscribe_into(&subscriber, &second, sub_qos(QoS::AtMostOnce)).await?;
    let publisher = env.plain("remap-pub").await?;
    let aliased = PublishOptions {
        qos: QoS::AtMostOnce,
        properties: PublishProperties {
            topic_alias: Some(1),
            ..Default::default()
        },
        ..Default::default()
    };
    let rounds = 20u32;
    for round in 0..rounds {
        for (topic, body) in [(&first, vec![b'1'; size]), (&second, b"2".to_vec())] {
            publisher
                .publish_with_options(topic, body.clone(), aliased.clone())
                .await
                .map_err(fail(&format!(
                    "alias-setting publish to {topic} in round {round}"
                )))?;
            publisher
                .publish_with_options("", body[..1].to_vec(), aliased.clone())
                .await
                .map_err(fail(&format!(
                    "alias-only publish for {topic} in round {round}"
                )))?;
        }
    }
    let expected = rounds as usize * 2;
    let total = wait_for_count(&[&first_inbox, &second_inbox], expected * 2).await;
    ensure(publisher.is_connected().await, || {
        format!(
            "publisher disconnected; subscriber saw {total}/{}",
            expected * 2
        )
    })?;
    for (topic, inbox, marker) in [(&first, &first_inbox, b'1'), (&second, &second_inbox, b'2')] {
        let messages = inbox.lock().unwrap().clone();
        ensure(messages.len() == expected, || {
            format!(
                "{topic}: expected {expected} deliveries, got {}",
                messages.len()
            )
        })?;
        ensure(
            messages.iter().all(|m| m.payload.first() == Some(&marker)),
            || format!("{topic} received a message published for the other topic"),
        )?;
    }
    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    Ok(())
}

async fn check_user_properties(env: Env) -> Result<(), String> {
    let topic = env.topic("props");
    let subscriber = env.plain("props-sub").await?;
    let inbox = subscribe_into(&subscriber, &topic, sub_qos(QoS::AtLeastOnce)).await?;
    let publisher = env.plain("props-pub").await?;
    let sent = vec![
        ("trace".to_string(), "abc-123".to_string()),
        ("k".to_string(), "v1".to_string()),
        ("k".to_string(), "v2".to_string()),
    ];
    let options = PublishOptions {
        qos: QoS::AtLeastOnce,
        properties: PublishProperties {
            user_properties: sent.clone(),
            content_type: Some("text/plain".to_string()),
            ..Default::default()
        },
        ..Default::default()
    };
    publisher
        .publish_with_options(&topic, "x", options)
        .await
        .map_err(fail("publish with user properties"))?;
    let total = wait_for_count(&[&inbox], 1).await;
    ensure(total == 1, || format!("expected one delivery, got {total}"))?;
    let message = inbox.lock().unwrap()[0].clone();
    let received: Vec<(String, String)> = message
        .properties
        .user_properties
        .iter()
        .filter(|(key, _)| !key.starts_with("x-mqtt-"))
        .cloned()
        .collect();
    ensure(received == sent, || {
        format!("user properties changed: sent {sent:?}, got {received:?}")
    })?;
    ensure(
        message.properties.content_type.as_deref() == Some("text/plain"),
        || "content type lost".to_string(),
    )?;
    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    Ok(())
}

async fn check_qos_flow(env: Env, qos: QoS) -> Result<(), String> {
    let topic = env.topic(&format!("qos{}", qos as u8));
    let subscriber = env.plain("qos-sub").await?;
    let inbox = subscribe_into(&subscriber, &topic, sub_qos(qos)).await?;
    let publisher = env.plain("qos-pub").await?;
    let count = 50u32;
    for i in 0..count {
        let result = publisher
            .publish_with_options(&topic, i.to_string(), pub_qos(qos))
            .await
            .map_err(fail("qos publish"))?;
        let completed = matches!(
            (qos, &result),
            (
                QoS::AtLeastOnce,
                PublishResult::Sent(Delivery::AtLeastOnce { .. })
            ) | (
                QoS::ExactlyOnce,
                PublishResult::Sent(Delivery::ExactlyOnce { .. })
            )
        );
        ensure(completed, || {
            format!("publish {i} did not complete its {qos:?} handshake: {result:?}")
        })?;
    }
    let total = wait_for_count(&[&inbox], count as usize).await;
    let messages = inbox.lock().unwrap().clone();
    let indices: Vec<u32> = messages.iter().filter_map(payload_index).collect();
    ensure(indices == (0..count).collect::<Vec<_>>(), || {
        format!("expected each of {count} messages exactly once in order, got {total}: {indices:?}")
    })?;
    ensure(messages.iter().all(|m| m.qos == qos), || {
        format!("a delivery was downgraded from {qos:?}")
    })?;
    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    Ok(())
}

async fn check_subscription_identifiers(env: Env) -> Result<(), String> {
    let subscriber = env.plain("sid-sub").await?;
    let inbox_a = subscribe_into(
        &subscriber,
        &env.topic("sid/a/+"),
        sub_qos(QoS::AtLeastOnce).with_subscription_identifier(11),
    )
    .await?;
    let inbox_b = subscribe_into(
        &subscriber,
        &env.topic("sid/b/+"),
        sub_qos(QoS::AtLeastOnce).with_subscription_identifier(22),
    )
    .await?;
    let publisher = env.plain("sid-pub").await?;
    for suffix in ["sid/a/1", "sid/b/1"] {
        publisher
            .publish_with_options(env.topic(suffix), "x", pub_qos(QoS::AtLeastOnce))
            .await
            .map_err(fail("sid publish"))?;
    }
    let total = wait_for_count(&[&inbox_a, &inbox_b], 2).await;
    ensure(total == 2, || format!("expected 2 deliveries, got {total}"))?;
    let ids_a = inbox_a.lock().unwrap()[0]
        .properties
        .subscription_identifiers
        .clone();
    let ids_b = inbox_b.lock().unwrap()[0]
        .properties
        .subscription_identifiers
        .clone();
    ensure(ids_a == vec![11] && ids_b == vec![22], || {
        format!("subscription identifiers wrong: a={ids_a:?} b={ids_b:?}")
    })?;
    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    Ok(())
}

#[tokio::test]
async fn shared_subscriptions_across_stream_strategies() {
    Box::pin(run_matrix("shared subscription", check_shared_subscription)).await;
}

#[tokio::test]
async fn request_response_across_stream_strategies() {
    Box::pin(run_matrix("request/response", check_request_response)).await;
}

#[tokio::test]
async fn retained_messages_across_stream_strategies() {
    Box::pin(run_matrix("retained", check_retained)).await;
}

#[tokio::test]
async fn will_message_across_stream_strategies() {
    Box::pin(run_matrix("will on abrupt disconnect", check_will)).await;
}

#[tokio::test]
async fn topic_alias_across_stream_strategies() {
    Box::pin(run_matrix("topic alias (QoS 1)", check_topic_alias)).await;
}

#[tokio::test]
async fn topic_alias_qos0_burst_across_stream_strategies() {
    Box::pin(run_matrix(
        "topic alias (QoS 0 burst)",
        check_topic_alias_qos0_burst,
    ))
    .await;
}

#[tokio::test]
async fn topic_alias_after_large_publish_across_stream_strategies() {
    Box::pin(run_matrix(
        "topic alias (alias-only right after a 512 KB establishing publish)",
        |env| check_topic_alias_after_large_publish(env, 512 * 1024),
    ))
    .await;
}

#[tokio::test]
async fn topic_alias_after_medium_publish_across_stream_strategies() {
    Box::pin(run_matrix(
        "topic alias (alias-only right after a 64 KB establishing publish)",
        |env| check_topic_alias_after_large_publish(env, 64 * 1024),
    ))
    .await;
}

#[tokio::test]
async fn topic_alias_remap_across_stream_strategies() {
    Box::pin(run_matrix(
        "topic alias (one alias remapped between two topics)",
        |env| check_topic_alias_remap(env, 16 * 1024),
    ))
    .await;
}

async fn check_topic_alias_with_datagrams(
    establish_size: usize,
    alias_only_qos: QoS,
) -> Result<(), String> {
    let link = Link::Quic {
        client: StreamStrategy::ControlOnly,
        broker: ServerDeliveryStrategy::ControlOnly,
    };
    let (shutdown, url) = start_broker(link).await;
    let env = Env {
        url: url.clone(),
        link,
        base: format!("v5/{}", Ulid::new()),
    };
    let outcome = Box::pin(async {
        let topic = env.topic("alias-datagram");
        let subscriber = env.plain("alias-dgram-sub").await?;
        let inbox = subscribe_into(&subscriber, &topic, sub_qos(QoS::AtLeastOnce)).await?;
        let options = ConnectOptions::new(format!("alias-dgram-pub-{}", Ulid::new()));
        let publisher = MqttClient::with_options(options.clone());
        publisher.set_insecure_tls(true).await;
        publisher.set_quic_datagrams(true).await;
        Box::pin(publisher.connect_with_options(&url, options))
            .await
            .map_err(fail("alias-dgram-pub connect"))?;
        let rounds = 40u16;
        let establishing = vec![b'L'; establish_size];
        for round in 0..rounds {
            let aliased = |qos| PublishOptions {
                qos,
                properties: PublishProperties {
                    topic_alias: Some(round % 10 + 1),
                    ..Default::default()
                },
                ..Default::default()
            };
            publisher
                .publish_with_options(&topic, establishing.clone(), aliased(QoS::AtMostOnce))
                .await
                .map_err(fail(&format!("alias-setting publish {round}")))?;
            publisher
                .publish_with_options("", "small", aliased(alias_only_qos))
                .await
                .map_err(fail(&format!("alias-only publish {round}")))?;
        }
        let expected = usize::from(rounds) * 2;
        let total = wait_for_count(&[&inbox], expected).await;
        ensure(publisher.is_connected().await, || {
            format!("publisher disconnected; subscriber saw {total}/{expected}")
        })?;
        ensure(total == expected, || {
            format!("expected {expected} deliveries, got {total}")
        })?;
        publisher.disconnect().await.ok();
        subscriber.disconnect().await.ok();
        Ok(())
    })
    .await;
    shutdown.shutdown();
    outcome
}

#[tokio::test]
async fn topic_alias_with_quic_datagrams() {
    let mut failures = Vec::new();
    for (establish_size, alias_only_qos, case) in [
        (
            16,
            QoS::AtLeastOnce,
            "datagram alias-setting, control-stream alias-only",
        ),
        (
            64 * 1024,
            QoS::AtMostOnce,
            "control-stream alias-setting, datagram alias-only",
        ),
    ] {
        match check_topic_alias_with_datagrams(establish_size, alias_only_qos).await {
            Ok(()) => println!("MATRIX | topic alias with datagrams | {case} | PASS"),
            Err(reason) => {
                println!("MATRIX | topic alias with datagrams | {case} | FAIL | {reason}");
                failures.push(format!("{case}: {reason}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "topic alias with datagrams failed: {failures:#?}"
    );
}

#[tokio::test]
async fn user_properties_across_stream_strategies() {
    Box::pin(run_matrix("user properties", check_user_properties)).await;
}

#[tokio::test]
async fn qos1_flow_across_stream_strategies() {
    Box::pin(run_matrix("QoS 1 flow", |env| {
        check_qos_flow(env, QoS::AtLeastOnce)
    }))
    .await;
}

#[tokio::test]
async fn qos2_flow_across_stream_strategies() {
    Box::pin(run_matrix("QoS 2 flow", |env| {
        check_qos_flow(env, QoS::ExactlyOnce)
    }))
    .await;
}

#[tokio::test]
async fn subscription_identifiers_across_stream_strategies() {
    Box::pin(run_matrix(
        "subscription identifiers",
        check_subscription_identifiers,
    ))
    .await;
}

struct OrderingReport {
    received: usize,
    duplicates: usize,
    inversions: usize,
    max_displacement: u32,
    streams: BTreeSet<Option<u64>>,
    publisher_max_streams_frames: Option<u64>,
}

fn analyse(
    sequence: &[(u32, Option<u64>)],
    publisher_max_streams_frames: Option<u64>,
) -> OrderingReport {
    let seen: BTreeSet<u32> = sequence.iter().map(|(seq, _)| *seq).collect();
    let streams: BTreeSet<Option<u64>> = sequence.iter().map(|(_, stream)| *stream).collect();
    let inversions = sequence.windows(2).filter(|w| w[1].0 < w[0].0).count();
    let mut highest = 0u32;
    let mut max_displacement = 0u32;
    for (seq, _) in sequence {
        highest = highest.max(*seq);
        max_displacement = max_displacement.max(highest - seq);
    }
    OrderingReport {
        received: sequence.len(),
        duplicates: sequence.len() - seen.len(),
        inversions,
        max_displacement,
        streams,
        publisher_max_streams_frames,
    }
}

async fn measure_ordering(env: Env, qos: QoS, count: u32) -> Result<OrderingReport, String> {
    let topic = env.topic("ordered");
    let subscriber = env.plain("order-sub").await?;
    let sequence: Arrivals = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&sequence);
    subscriber
        .subscribe_with_options(&topic, sub_qos(qos), move |message| {
            if let Ok(bytes) = <[u8; 4]>::try_from(message.payload.as_slice()) {
                sink.lock()
                    .unwrap()
                    .push((u32::from_be_bytes(bytes), message.stream_id));
            }
        })
        .await
        .map_err(fail("ordering subscribe"))?;
    let publisher = env.plain("order-pub").await?;
    for seq in 0..count {
        publisher
            .publish_with_options(&topic, seq.to_be_bytes().to_vec(), pub_qos(qos))
            .await
            .map_err(fail("ordering publish"))?;
    }
    let mut last = 0;
    for _ in 0..200 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let now = sequence.lock().unwrap().len();
        if now == count as usize || (now == last && now > 0) {
            break;
        }
        last = now;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let publisher_max_streams_frames = publisher.quic_connection().await.map(|connection| {
        let frames = connection.stats().frame_rx;
        frames.max_streams_bidi + frames.max_streams_uni
    });
    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();
    let snapshot = sequence.lock().unwrap().clone();
    Ok(analyse(&snapshot, publisher_max_streams_frames))
}

#[tokio::test]
async fn per_topic_ordering_on_loopback() {
    let mut failures = Vec::new();
    for mode in MODES {
        for (qos, count) in [(QoS::AtMostOnce, 5000u32), (QoS::AtLeastOnce, 3000u32)] {
            let (shutdown, url) = start_broker(mode.link).await;
            let env = Env {
                url,
                link: mode.link,
                base: format!("order/{}", Ulid::new()),
            };
            let report = Box::pin(measure_ordering(env, qos, count)).await;
            shutdown.shutdown();
            match report {
                Ok(r) => {
                    let stream_ids = if r.streams.len() <= 2 {
                        format!("{:?}", r.streams)
                    } else {
                        "many".to_string()
                    };
                    println!(
                        "ORDERING | {} | {qos:?} | sent={count} received={} duplicates={} inversions={} max_displacement={} subscriber_distinct_streams={} subscriber_stream_ids={stream_ids} publisher_max_streams_frames_rx={:?}",
                        mode.name,
                        r.received,
                        r.duplicates,
                        r.inversions,
                        r.max_displacement,
                        r.streams.len(),
                        r.publisher_max_streams_frames
                    );
                    let complete = qos == QoS::AtMostOnce || r.received == count as usize;
                    let expected_streams = match mode.link {
                        Link::Quic {
                            broker: ServerDeliveryStrategy::PerPublish,
                            ..
                        } => r.received,
                        _ => 1,
                    };
                    if !complete || r.duplicates > 0 || r.streams.len() != expected_streams {
                        failures.push(format!(
                            "{} {qos:?}: received {} of {count}, {} duplicates, {} subscriber streams (expected {expected_streams})",
                            mode.name,
                            r.received,
                            r.duplicates,
                            r.streams.len()
                        ));
                    }
                }
                Err(reason) => failures.push(format!("{} {qos:?}: {reason}", mode.name)),
            }
        }
    }
    assert!(failures.is_empty(), "ordering runs failed: {failures:#?}");
}
