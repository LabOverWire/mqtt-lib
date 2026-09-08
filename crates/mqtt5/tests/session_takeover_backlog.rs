#![cfg(feature = "broker")]
use mqtt5::broker::config::{StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::{AckToken, ConnectOptions, MqttClient, QoS, SubscribeOptions};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ulid::Ulid;

const CHANNEL_CAPACITY: usize = 4;
const FLOOD: usize = 200;
const HELD_WINDOW: u16 = 2;

async fn start_broker() -> (String, tokio::task::JoinHandle<mqtt5::error::Result<()>>) {
    let config = BrokerConfig::default()
        .with_bind_address(([127, 0, 0, 1], 0))
        .with_client_channel_capacity(CHANNEL_CAPACITY)
        .with_storage(StorageConfig::default().with_backend(StorageBackend::Memory));
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

fn qos1() -> SubscribeOptions {
    SubscribeOptions {
        qos: QoS::AtLeastOnce,
        ..Default::default()
    }
}

fn persistent(client_id: &str, clean_start: bool) -> ConnectOptions {
    ConnectOptions::new(client_id)
        .with_clean_start(clean_start)
        .with_session_expiry_interval(3600)
        .with_automatic_reconnect(false)
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

struct FirstConnection {
    client: MqttClient,
    received: Arc<Mutex<Vec<usize>>>,
    tokens: Arc<Mutex<Vec<AckToken>>>,
}

async fn connect_holding_window(url: &str, client_id: &str, topic: &str) -> FirstConnection {
    let received = Arc::new(Mutex::new(Vec::new()));
    let tokens: Arc<Mutex<Vec<AckToken>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let held = Arc::clone(&tokens);
    let client = MqttClient::with_options(
        persistent(client_id, false)
            .with_deferred_ack(true)
            .with_receive_maximum(HELD_WINDOW),
    );
    client.connect(url).await.unwrap();
    client
        .subscribe_with_ack(topic.to_string(), qos1(), move |publish, token| {
            sink.lock().unwrap().push(payload_index(&publish.payload));
            held.lock().unwrap().push(token);
        })
        .await
        .unwrap();
    FirstConnection {
        client,
        received,
        tokens,
    }
}

async fn flood(url: &str, topic: &str) -> MqttClient {
    let publisher = MqttClient::new(format!("takeover-pub-{}", Ulid::new()));
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
    .expect("a subscriber holding its window must not stall the publisher");
    publisher
}

#[tokio::test]
async fn takeover_resumes_the_backlog_in_order_without_loss() {
    let (url, broker_task) = start_broker().await;
    let client_id = format!("takeover-{}", Ulid::new());
    let topic = format!("takeover/{}", Ulid::new());

    let first = connect_holding_window(&url, &client_id, &topic).await;
    let publisher = flood(&url, &topic).await;
    assert!(
        wait_until(Duration::from_secs(5), || {
            first.received.lock().unwrap().len() == usize::from(HELD_WINDOW)
        })
        .await,
        "the first connection holds exactly its window"
    );

    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let second = MqttClient::with_options(persistent(&client_id, false));
    second.connect(&url).await.unwrap();
    second
        .subscribe_with_options(topic.clone(), qos1(), move |msg| {
            sink.lock().unwrap().push(payload_index(&msg.payload));
        })
        .await
        .unwrap();

    assert!(
        wait_until(Duration::from_secs(30), || {
            received.lock().unwrap().len() >= FLOOD
        })
        .await,
        "the second connection drains the whole backlog"
    );
    let delivered = received.lock().unwrap().clone();
    let expected: Vec<usize> = (0..FLOOD).collect();
    assert_eq!(
        delivered, expected,
        "unacked messages are redelivered first, then the queue, in publish order"
    );
    assert!(
        first.tokens.lock().unwrap().len() == usize::from(HELD_WINDOW),
        "the displaced connection still holds its tokens"
    );
    assert!(
        !first.client.is_connected().await,
        "the displaced connection was told its session was taken over"
    );

    second.disconnect().await.unwrap();
    publisher.disconnect().await.unwrap();
    drop(first);
    broker_task.abort();
}

#[tokio::test]
async fn reconnect_right_after_close_resumes_the_backlog() {
    let (url, broker_task) = start_broker().await;
    let client_id = format!("takeover-{}", Ulid::new());
    let topic = format!("takeover/{}", Ulid::new());

    let first = connect_holding_window(&url, &client_id, &topic).await;
    let publisher = flood(&url, &topic).await;
    assert!(
        wait_until(Duration::from_secs(5), || {
            first.received.lock().unwrap().len() == usize::from(HELD_WINDOW)
        })
        .await,
        "the first connection holds exactly its window"
    );

    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let second = MqttClient::with_options(persistent(&client_id, false));
    let closing = tokio::spawn(async move { first.client.disconnect().await });
    second.connect(&url).await.unwrap();
    closing.await.unwrap().ok();
    second
        .subscribe_with_options(topic.clone(), qos1(), move |msg| {
            sink.lock().unwrap().push(payload_index(&msg.payload));
        })
        .await
        .unwrap();

    let complete = wait_until(Duration::from_secs(30), || {
        received.lock().unwrap().len() >= FLOOD
    })
    .await;
    let delivered = received.lock().unwrap().clone();
    assert!(
        complete,
        "a connection racing the previous close still gets the whole backlog; got {} of {FLOOD}, first {:?}, last {:?}",
        delivered.len(),
        delivered.first(),
        delivered.last()
    );
    let expected: Vec<usize> = (0..FLOOD).collect();
    assert_eq!(delivered, expected);

    second.disconnect().await.unwrap();
    publisher.disconnect().await.unwrap();
    drop(first.tokens);
    broker_task.abort();
}

#[tokio::test]
async fn clean_start_takeover_discards_the_backlog_and_subscriptions() {
    let (url, broker_task) = start_broker().await;
    let client_id = format!("takeover-{}", Ulid::new());
    let topic = format!("takeover/{}", Ulid::new());

    let first = connect_holding_window(&url, &client_id, &topic).await;
    let publisher = flood(&url, &topic).await;
    assert!(
        wait_until(Duration::from_secs(5), || {
            first.received.lock().unwrap().len() == usize::from(HELD_WINDOW)
        })
        .await,
        "the first connection holds exactly its window"
    );

    let received = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&received);
    let second = MqttClient::with_options(persistent(&client_id, true));
    second.connect(&url).await.unwrap();
    second
        .subscribe_with_options(topic.clone(), qos1(), move |msg| {
            sink.lock().unwrap().push(payload_index(&msg.payload));
        })
        .await
        .unwrap();

    publisher
        .publish_qos1(&topic, format!("msg-{FLOOD}").into_bytes())
        .await
        .unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || {
            received.lock().unwrap().contains(&FLOOD)
        })
        .await,
        "a message published after the clean start reaches the new connection"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        received.lock().unwrap().clone(),
        vec![FLOOD],
        "nothing from the old session's backlog reaches a clean-start connection"
    );

    second.disconnect().await.unwrap();
    publisher.disconnect().await.unwrap();
    drop(first);
    broker_task.abort();
}
