#![cfg(feature = "broker")]
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::MqttBroker;
use mqtt5::{ConnectOptions, MqttClient, QoS, SubscribeOptions};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Barrier;

const ROUNDS: usize = 40;
const PAIRS: usize = 8;
const SUBSCRIBER: &str = "order-subscriber";
const PUBLISHER: &str = "order-publisher";
const DELIVERY_WITHIN: Duration = Duration::from_secs(3);

struct Trial {
    topic: String,
    callbacks: Arc<AtomicUsize>,
    server_holds: bool,
}

async fn start_broker() -> (String, Arc<MessageRouter>) {
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: false,
            ..Default::default()
        });
    let mut broker = MqttBroker::with_config(config).await.unwrap();
    let url = format!("mqtt://{}", broker.local_addr().unwrap());
    let router = broker.router();
    let mut ready = broker.ready_receiver();
    tokio::spawn(async move {
        let _ = broker.run().await;
    });
    ready.wait_for(|&is_ready| is_ready).await.unwrap();
    (url, router)
}

fn without_session(id: &str) -> ConnectOptions {
    ConnectOptions::new(id)
        .with_clean_start(true)
        .with_session_expiry_interval(0)
        .with_automatic_reconnect(false)
}

async fn subscribe_while_unsubscribing(client: &MqttClient, round: usize) -> Vec<Trial> {
    let barrier = Arc::new(Barrier::new(PAIRS * 2));
    let mut racing = Vec::with_capacity(PAIRS);
    for pair in 0..PAIRS {
        let topic = format!("t/{round}/{pair}");
        let callbacks = Arc::new(AtomicUsize::new(0));
        let subscribing = client.clone();
        let subscribe_topic = topic.clone();
        let subscribe_barrier = Arc::clone(&barrier);
        let counted = Arc::clone(&callbacks);
        let subscribe = tokio::spawn(async move {
            subscribe_barrier.wait().await;
            subscribing
                .subscribe_with_options(
                    subscribe_topic,
                    SubscribeOptions {
                        qos: QoS::AtLeastOnce,
                        ..Default::default()
                    },
                    move |_message| {
                        counted.fetch_add(1, Ordering::SeqCst);
                    },
                )
                .await
        });
        let unsubscribing = client.clone();
        let unsubscribe_topic = topic.clone();
        let unsubscribe_barrier = Arc::clone(&barrier);
        let unsubscribe = tokio::spawn(async move {
            unsubscribe_barrier.wait().await;
            unsubscribing.unsubscribe(unsubscribe_topic).await
        });
        racing.push((topic, callbacks, subscribe, unsubscribe));
    }
    let mut trials = Vec::with_capacity(PAIRS);
    for (topic, callbacks, subscribe, unsubscribe) in racing {
        subscribe.await.unwrap().unwrap();
        unsubscribe.await.unwrap().unwrap();
        trials.push(Trial {
            topic,
            callbacks,
            server_holds: false,
        });
    }
    trials
}

async fn wait_for_deliveries(trials: &[Trial]) {
    let started = Instant::now();
    while started.elapsed() < DELIVERY_WITHIN
        && trials
            .iter()
            .any(|trial| trial.server_holds && trial.callbacks.load(Ordering::SeqCst) == 0)
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn racing_subscribe_and_unsubscribe_leave_the_client_agreeing_with_the_server() {
    let (url, router) = start_broker().await;
    let client = MqttClient::new(SUBSCRIBER);
    client
        .connect_with_options(&url, without_session(SUBSCRIBER))
        .await
        .unwrap();
    let publisher = MqttClient::new(PUBLISHER);
    publisher
        .connect_with_options(&url, without_session(PUBLISHER))
        .await
        .unwrap();

    let mut all = Vec::with_capacity(ROUNDS * PAIRS);
    for round in 0..ROUNDS {
        let mut trials = subscribe_while_unsubscribing(&client, round).await;
        for trial in &mut trials {
            trial.server_holds = router.has_subscription(SUBSCRIBER, &trial.topic).await;
            if trial.server_holds {
                publisher
                    .publish_qos1(trial.topic.clone(), b"x".to_vec())
                    .await
                    .unwrap();
            }
        }
        wait_for_deliveries(&trials).await;
        all.extend(trials);
    }
    let silent: Vec<&str> = all
        .iter()
        .filter(|trial| trial.server_holds && trial.callbacks.load(Ordering::SeqCst) == 0)
        .map(|trial| trial.topic.as_str())
        .collect();

    client.disconnect().await.unwrap();
    client
        .connect_with_options(&url, without_session(SUBSCRIBER))
        .await
        .unwrap();
    let started = Instant::now();
    let mut disagreeing = Vec::new();
    loop {
        disagreeing.clear();
        for trial in &all {
            if router.has_subscription(SUBSCRIBER, &trial.topic).await != trial.server_holds {
                disagreeing.push(trial.topic.as_str());
            }
        }
        if disagreeing.is_empty() || started.elapsed() > DELIVERY_WITHIN {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(
        all.iter().any(|trial| trial.server_holds) && all.iter().any(|trial| !trial.server_holds),
        "the race must end both ways across the trials"
    );
    assert!(
        silent.is_empty(),
        "subscriptions the server holds deliver to no callback: {silent:?}"
    );
    assert!(
        disagreeing.is_empty(),
        "a reconnect restored subscriptions the server had removed or dropped ones it held: {disagreeing:?}"
    );
}
