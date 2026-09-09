#![cfg(feature = "broker")]
use mqtt5::broker::config::StorageConfig;
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::{AckToken, ConnectOptions, MqttClient, PublishOptions, QoS, SubscribeOptions};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ulid::Ulid;

const CHANNEL_CAPACITY: usize = 4;
const FLOOD: usize = 300;

fn client_id(name: &str) -> String {
    format!("backlog-{name}-{}", Ulid::new())
}

async fn start_broker() -> (String, tokio::task::JoinHandle<mqtt5::error::Result<()>>) {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_client_channel_capacity(CHANNEL_CAPACITY)
        .with_storage(StorageConfig::default().with_persistence(false));
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let port = broker.local_addr().unwrap().port();
    let task = tokio::spawn(async move { broker.run().await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    (format!("mqtt://127.0.0.1:{port}"), task)
}

fn payload_index(payload: &[u8]) -> usize {
    std::str::from_utf8(payload)
        .unwrap()
        .strip_prefix("msg-")
        .unwrap()
        .parse()
        .unwrap()
}

async fn flood(url: &str, topic: &str) -> MqttClient {
    let publisher = MqttClient::new(client_id("pub"));
    publisher.connect(url).await.unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        for index in 0..FLOOD {
            publisher
                .publish_qos1(topic, format!("msg-{index}").into_bytes())
                .await
                .unwrap();
        }
    })
    .await
    .expect("a slow subscriber must not stall the publisher");
    publisher
}

async fn wait_until(deadline: Duration, cond: impl Fn() -> bool) -> bool {
    let started = tokio::time::Instant::now();
    while started.elapsed() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

fn assert_in_order(received: &[usize]) {
    let expected: Vec<usize> = (0..FLOOD).collect();
    assert_eq!(received, expected.as_slice());
}

#[tokio::test]
async fn flooded_qos1_subscriber_receives_every_message_in_order() {
    let (url, broker_task) = start_broker().await;
    let topic = format!("flood/{}", Ulid::new());

    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let subscriber = MqttClient::new(client_id("sub"));
    subscriber.connect(&url).await.unwrap();
    subscriber
        .subscribe_with_options(
            topic.clone(),
            SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |msg| sink.lock().unwrap().push(payload_index(&msg.payload)),
        )
        .await
        .unwrap();

    let publisher = flood(&url, &topic).await;

    assert!(
        wait_until(Duration::from_secs(30), || {
            received.lock().unwrap().len() == FLOOD
        })
        .await,
        "every queued message must be drained while the subscriber stays connected"
    );
    assert_in_order(&received.lock().unwrap());

    subscriber.disconnect().await.unwrap();
    publisher.disconnect().await.unwrap();
    broker_task.abort();
}

#[tokio::test]
async fn deferred_ack_subscriber_backlog_drains_as_the_window_frees() {
    let (url, broker_task) = start_broker().await;
    let topic = format!("flood/{}", Ulid::new());

    let received = Arc::new(Mutex::new(Vec::new()));
    let tokens: Arc<Mutex<Vec<AckToken>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let held = Arc::clone(&tokens);
    let subscriber = MqttClient::with_options(
        ConnectOptions::new(client_id("sub"))
            .with_deferred_ack(true)
            .with_clean_start(false)
            .with_session_expiry_interval(3600)
            .with_receive_maximum(2),
    );
    subscriber.connect(&url).await.unwrap();
    subscriber
        .subscribe_with_ack(
            topic.clone(),
            SubscribeOptions {
                qos: QoS::AtLeastOnce,
                ..Default::default()
            },
            move |publish, token| {
                sink.lock().unwrap().push(payload_index(&publish.payload));
                held.lock().unwrap().push(token);
            },
        )
        .await
        .unwrap();

    let publisher = flood(&url, &topic).await;

    assert!(
        wait_until(Duration::from_secs(5), || received.lock().unwrap().len()
            == 2)
        .await,
        "the window admits receive_maximum messages"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        received.lock().unwrap().len(),
        2,
        "no message may be sent beyond the window while acks are withheld"
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        publisher
            .publish_with_options(
                "probe/alive",
                b"alive".to_vec(),
                PublishOptions {
                    qos: QoS::AtLeastOnce,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    })
    .await
    .expect("the broker stays responsive with a full backlog");

    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let before = received.lock().unwrap().len();
            if before == FLOOD {
                break;
            }
            let batch: Vec<AckToken> = tokens.lock().unwrap().drain(..).collect();
            for token in batch {
                token.ack();
            }
            assert!(
                wait_until(Duration::from_secs(5), || {
                    received.lock().unwrap().len() > before
                })
                .await,
                "acking frees the window and the backlog keeps flowing"
            );
        }
    })
    .await
    .expect("the whole backlog drains through the window");
    assert_in_order(&received.lock().unwrap());

    subscriber.disconnect().await.unwrap();
    publisher.disconnect().await.unwrap();
    broker_task.abort();
}
