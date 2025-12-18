use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum BenchMode {
    #[default]
    Throughput,
    Latency,
}

#[derive(Args)]
pub struct BenchCommand {
    #[arg(long, value_enum, default_value = "throughput")]
    pub mode: BenchMode,

    #[arg(long, default_value = "10")]
    pub duration: u64,

    #[arg(long, default_value = "2")]
    pub warmup: u64,

    #[arg(long, default_value = "64")]
    pub payload_size: usize,

    #[arg(long, short, default_value = "bench/test")]
    pub topic: String,

    #[arg(long, short, default_value = "0", value_parser = parse_qos)]
    pub qos: QoS,

    #[arg(long, short = 'U', conflicts_with_all = &["host", "port"])]
    pub url: Option<String>,

    #[arg(long, short = 'H', default_value = "localhost")]
    pub host: String,

    #[arg(long, short, default_value = "1883")]
    pub port: u16,

    #[arg(long, short)]
    pub client_id: Option<String>,

    #[arg(long, default_value = "1")]
    pub publishers: usize,

    #[arg(long, default_value = "1")]
    pub subscribers: usize,
}

fn parse_qos(s: &str) -> Result<QoS, String> {
    match s {
        "0" => Ok(QoS::AtMostOnce),
        "1" => Ok(QoS::AtLeastOnce),
        "2" => Ok(QoS::ExactlyOnce),
        _ => Err(format!("QoS must be 0, 1, or 2, got: {s}")),
    }
}

#[derive(Serialize)]
struct BenchConfig {
    duration_secs: u64,
    warmup_secs: u64,
    payload_size: usize,
    qos: u8,
    topic: String,
    publishers: usize,
    subscribers: usize,
}

#[derive(Serialize)]
struct ThroughputResults {
    published: u64,
    received: u64,
    elapsed_secs: f64,
    throughput_avg: f64,
    samples: Vec<u64>,
}

#[derive(Serialize)]
struct BenchOutput {
    mode: String,
    config: BenchConfig,
    results: ThroughputResults,
}

pub async fn execute(cmd: BenchCommand, verbose: bool, debug: bool) -> Result<()> {
    crate::init_basic_tracing(verbose, debug);

    match cmd.mode {
        BenchMode::Throughput => run_throughput(cmd).await,
        BenchMode::Latency => {
            eprintln!("Latency mode not yet implemented");
            Ok(())
        }
    }
}

async fn run_throughput(cmd: BenchCommand) -> Result<()> {
    let broker_url = cmd
        .url
        .clone()
        .unwrap_or_else(|| format!("mqtt://{}:{}", cmd.host, cmd.port));

    let base_client_id = cmd
        .client_id
        .clone()
        .unwrap_or_else(|| format!("mqttv5-bench-{}", rand::rng().random::<u32>()));

    eprintln!(
        "connecting {} publisher(s) and {} subscriber(s) to {}...",
        cmd.publishers, cmd.subscribers, broker_url
    );

    let mut pub_clients = Vec::with_capacity(cmd.publishers);
    for i in 0..cmd.publishers {
        let pub_client_id = format!("{base_client_id}-pub-{i}");
        let pub_client = MqttClient::new(&pub_client_id);
        let pub_options = ConnectOptions::new(pub_client_id)
            .with_clean_start(true)
            .with_keep_alive(Duration::from_secs(30));

        pub_client
            .connect_with_options(&broker_url, pub_options)
            .await
            .context("failed to connect publisher")?;
        pub_clients.push(pub_client);
    }

    let received = Arc::new(AtomicU64::new(0));
    let topic = cmd.topic.clone();

    let mut sub_clients = Vec::with_capacity(cmd.subscribers);
    for i in 0..cmd.subscribers {
        let sub_client_id = format!("{base_client_id}-sub-{i}");
        let sub_client = MqttClient::new(&sub_client_id);
        let sub_options = ConnectOptions::new(sub_client_id)
            .with_clean_start(true)
            .with_keep_alive(Duration::from_secs(30));

        sub_client
            .connect_with_options(&broker_url, sub_options)
            .await
            .context("failed to connect subscriber")?;

        let received_clone = Arc::clone(&received);
        sub_client
            .subscribe(&topic, move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .context("failed to subscribe")?;

        sub_clients.push(sub_client);
    }

    eprintln!("subscribed {} client(s) to {}", cmd.subscribers, topic);

    let payload: Arc<[u8]> = vec![0u8; cmd.payload_size].into();
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let published = Arc::new(AtomicU64::new(0));

    eprintln!("warming up for {}s...", cmd.warmup);
    let warmup_duration = Duration::from_secs(cmd.warmup);
    let measure_duration = Duration::from_secs(cmd.duration);

    let mut handles = Vec::with_capacity(cmd.publishers);
    for pub_client in pub_clients {
        let topic = topic.clone();
        let payload = Arc::clone(&payload);
        let running = Arc::clone(&running);
        let published = Arc::clone(&published);
        let qos = cmd.qos;

        handles.push(tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                if publish_message(&pub_client, &topic, &payload, qos)
                    .await
                    .is_ok()
                {
                    published.fetch_add(1, Ordering::Relaxed);
                }
            }
            pub_client.disconnect().await.ok();
        }));
    }

    tokio::time::sleep(warmup_duration).await;

    received.store(0, Ordering::SeqCst);
    published.store(0, Ordering::SeqCst);
    let mut samples: Vec<u64> = Vec::new();
    let mut last_received = 0u64;

    eprintln!("measuring for {}s...", cmd.duration);
    let measure_start = Instant::now();
    let measure_end = measure_start + measure_duration;
    let mut next_sample = measure_start + Duration::from_secs(1);

    while Instant::now() < measure_end {
        tokio::time::sleep(Duration::from_millis(10)).await;

        if Instant::now() >= next_sample {
            let current = received.load(Ordering::Relaxed);
            let delta = current - last_received;
            samples.push(delta);
            eprintln!("  {} msg/s", delta);
            last_received = current;
            next_sample += Duration::from_secs(1);
        }
    }

    running.store(false, Ordering::SeqCst);
    for handle in handles {
        handle.await.ok();
    }

    tokio::time::sleep(Duration::from_millis(100)).await;

    let total_published = published.load(Ordering::Relaxed);
    let total_received = received.load(Ordering::Relaxed);
    let elapsed = measure_start.elapsed().as_secs_f64();
    let throughput_avg = total_received as f64 / elapsed;

    let output = BenchOutput {
        mode: "throughput".to_string(),
        config: BenchConfig {
            duration_secs: cmd.duration,
            warmup_secs: cmd.warmup,
            payload_size: cmd.payload_size,
            qos: cmd.qos as u8,
            topic: cmd.topic,
            publishers: cmd.publishers,
            subscribers: cmd.subscribers,
        },
        results: ThroughputResults {
            published: total_published,
            received: total_received,
            elapsed_secs: elapsed,
            throughput_avg,
            samples,
        },
    };

    println!("{}", serde_json::to_string_pretty(&output)?);

    for sub_client in sub_clients {
        sub_client.disconnect().await.ok();
    }
    Ok(())
}

async fn publish_message(client: &MqttClient, topic: &str, payload: &[u8], qos: QoS) -> Result<()> {
    match qos {
        QoS::AtMostOnce => client.publish(topic, payload.to_vec()).await?,
        QoS::AtLeastOnce => client.publish_qos1(topic, payload.to_vec()).await?,
        QoS::ExactlyOnce => client.publish_qos2(topic, payload.to_vec()).await?,
    };
    Ok(())
}

use rand::Rng;
