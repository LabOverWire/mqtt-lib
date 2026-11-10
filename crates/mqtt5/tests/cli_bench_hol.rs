#![cfg(feature = "broker")]

mod common;
use common::cli_helpers::run_cli_command;
use common::TestBroker;
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::{BrokerEventHandler, ClientSubscribeEvent};
use mqtt5::QoS;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

type SeenSubscriptions = Arc<Mutex<Vec<(String, String, QoS)>>>;

struct SubscriptionRecorder {
    seen: SeenSubscriptions,
}

impl BrokerEventHandler for SubscriptionRecorder {
    fn on_client_subscribe<'a>(
        &'a self,
        event: ClientSubscribeEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let mut seen = self.seen.lock().unwrap();
            for subscription in event.subscriptions {
                seen.push((
                    event.client_id.to_string(),
                    subscription.topic_filter.to_string(),
                    subscription.qos,
                ));
            }
        })
    }
}

async fn recording_broker() -> (TestBroker, SeenSubscriptions) {
    let seen: SeenSubscriptions = Arc::new(Mutex::new(Vec::new()));
    let config = BrokerConfig::default()
        .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().unwrap())
        .with_storage(StorageConfig {
            backend: StorageBackend::Memory,
            enable_persistence: false,
            ..Default::default()
        })
        .with_event_handler(Arc::new(SubscriptionRecorder {
            seen: Arc::clone(&seen),
        }));
    (TestBroker::start_with_config(config).await, seen)
}

async fn run_hol(broker_url: &str, extra: &[&str]) -> serde_json::Value {
    run_hol_at(broker_url, "1", "400", extra).await
}

async fn run_hol_at(
    broker_url: &str,
    duration: &str,
    rate: &str,
    extra: &[&str],
) -> serde_json::Value {
    let mut args = vec![
        "bench",
        "--url",
        broker_url,
        "--mode",
        "hol-blocking",
        "--duration",
        duration,
        "--warmup",
        "0",
        "--topics",
        "4",
        "--sub-connections",
        "2",
        "--rate",
        rate,
    ];
    args.extend_from_slice(extra);
    let result = run_cli_command(&args).await;
    assert!(result.success, "bench failed: {}", result.stderr);
    serde_json::from_str(result.stdout.trim())
        .unwrap_or_else(|e| panic!("bench stdout is not JSON ({e}): {}", result.stdout))
}

fn subscriber_filters(seen: &SeenSubscriptions) -> Vec<(String, String, QoS)> {
    let mut filters: Vec<(String, String, QoS)> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(client_id, _, _)| client_id.contains("-sub-"))
        .cloned()
        .collect();
    filters.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    filters
}

fn trace_rows(trace_dir: &std::path::Path) -> (String, Vec<Vec<u64>>) {
    let csv = std::fs::read_to_string(trace_dir.join("messages.csv")).unwrap();
    let mut lines = csv.lines();
    let header = lines.next().unwrap().to_string();
    let rows = lines
        .map(|line| line.split(',').map(|v| v.parse().unwrap()).collect())
        .collect();
    (header, rows)
}

#[tokio::test]
async fn hol_subscribers_use_the_requested_qos() {
    let (broker, seen) = recording_broker().await;
    let json = run_hol(broker.address(), &["--qos", "1"]).await;
    assert_eq!(json["config"]["qos"], 1);

    let filters = subscriber_filters(&seen);
    assert_eq!(filters.len(), 4, "one subscription per topic: {filters:?}");
    for (client_id, filter, qos) in &filters {
        assert_eq!(
            *qos,
            QoS::AtLeastOnce,
            "{client_id} subscribed to {filter} below --qos 1"
        );
    }
}

#[tokio::test]
async fn hol_wildcard_makes_one_subscription_per_connection_and_keeps_topic_indices() {
    let (broker, seen) = recording_broker().await;
    let trace_dir = tempfile::tempdir().unwrap();
    let trace_path = trace_dir.path().to_str().unwrap();
    let json = run_hol(
        broker.address(),
        &["--qos", "1", "--hol-wildcard", "--trace-dir", trace_path],
    )
    .await;
    assert_eq!(json["config"]["hol_wildcard"], true);

    let filters = subscriber_filters(&seen);
    assert_eq!(
        filters.len(),
        2,
        "one subscription per connection: {filters:?}"
    );
    for (client_id, filter, qos) in &filters {
        assert_eq!(filter, "bench/hol/#", "{client_id} subscribed to {filter}");
        assert_eq!(*qos, QoS::AtLeastOnce);
    }

    let (header, rows) = trace_rows(trace_dir.path());
    assert_eq!(
        header,
        "topic_idx,seq,publish_ns,receive_ns,latency_us,stream_id,conn_idx"
    );
    assert!(!rows.is_empty(), "no messages traced");
    let mut topics_seen = [false; 4];
    for row in &rows {
        let topic_idx = usize::try_from(row[0]).unwrap();
        topics_seen[topic_idx] = true;
    }
    assert_eq!(topics_seen, [true; 4], "every topic must be traced");

    let per_topic = json["results"]["topics"].as_array().unwrap();
    assert_eq!(per_topic.len(), 4);
    for (i, topic) in per_topic.iter().enumerate() {
        assert_eq!(topic["topic"], format!("bench/hol/{i}"));
        assert!(
            topic["messages"].as_u64().unwrap() > 0,
            "topic {i} received nothing: {topic}"
        );
    }
}

#[tokio::test]
async fn hol_config_omits_the_new_flags_when_unset() {
    let (broker, _seen) = recording_broker().await;
    let json = run_hol(broker.address(), &[]).await;
    let config = json["config"].as_object().unwrap();
    assert!(!config.contains_key("hol_wildcard"));
    assert!(!config.contains_key("hol_subscription_streams"));
}

#[tokio::test]
async fn unpaced_qos0_publishes_keep_each_topics_order() {
    let (broker, _) = recording_broker().await;
    let trace_dir = tempfile::tempdir().unwrap();
    let trace_path = trace_dir.path().to_str().unwrap();
    run_hol_at(
        broker.address(),
        "2",
        "0",
        &["--qos", "0", "--trace-dir", trace_path],
    )
    .await;

    let (_, mut rows) = trace_rows(trace_dir.path());
    assert!(rows.len() > 1000, "too few messages traced: {}", rows.len());
    rows.sort_by_key(|row| row[3]);
    let mut highest: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();
    let mut out_of_order = 0usize;
    for row in &rows {
        let (topic_idx, seq) = (row[0], row[1]);
        if let Some(previous) = highest.get(&topic_idx) {
            if seq < *previous {
                out_of_order += 1;
            }
        }
        let entry = highest.entry(topic_idx).or_insert(seq);
        *entry = (*entry).max(seq);
    }
    assert_eq!(
        out_of_order,
        0,
        "{out_of_order} of {} messages arrived after a later message on their topic",
        rows.len()
    );
}
