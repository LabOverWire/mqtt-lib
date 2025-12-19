use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use rand::Rng;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum BenchMode {
    #[default]
    Throughput,
    Latency,
    Connections,
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

    #[arg(
        long,
        short = 'f',
        help = "Topic filter for subscriptions (defaults to topic)"
    )]
    pub filter: Option<String>,

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

    #[arg(long, default_value = "10")]
    pub concurrency: usize,
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
    filter: String,
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
struct LatencyResults {
    messages: u64,
    min_us: u64,
    max_us: u64,
    avg_us: f64,
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
    samples: Vec<u64>,
}

#[derive(Serialize)]
struct ConnectionResults {
    total_connections: u64,
    successful: u64,
    failed: u64,
    elapsed_secs: f64,
    connections_per_sec: f64,
    avg_connect_us: f64,
    p50_connect_us: u64,
    p95_connect_us: u64,
    p99_connect_us: u64,
    samples: Vec<u64>,
}

#[derive(Serialize)]
#[serde(untagged)]
enum BenchResults {
    Throughput(ThroughputResults),
    Latency(LatencyResults),
    Connections(ConnectionResults),
}

#[derive(Serialize)]
struct BenchOutput {
    mode: String,
    config: BenchConfig,
    results: BenchResults,
}

pub async fn execute(cmd: BenchCommand, verbose: bool, debug: bool) -> Result<()> {
    crate::init_basic_tracing(verbose, debug);

    match cmd.mode {
        BenchMode::Throughput => run_throughput(cmd).await,
        BenchMode::Latency => run_latency(cmd).await,
        BenchMode::Connections => run_connections(cmd).await,
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
    let filter = cmd.filter.clone().unwrap_or_else(|| topic.clone());

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
            .subscribe(&filter, move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .context("failed to subscribe")?;

        sub_clients.push(sub_client);
    }

    eprintln!("subscribed {} client(s) to {}", cmd.subscribers, filter);

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
            filter,
            publishers: cmd.publishers,
            subscribers: cmd.subscribers,
        },
        results: BenchResults::Throughput(ThroughputResults {
            published: total_published,
            received: total_received,
            elapsed_secs: elapsed,
            throughput_avg,
            samples,
        }),
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

async fn run_latency(cmd: BenchCommand) -> Result<()> {
    use std::sync::Mutex;
    use std::time::SystemTime;

    let broker_url = cmd
        .url
        .clone()
        .unwrap_or_else(|| format!("mqtt://{}:{}", cmd.host, cmd.port));

    let base_client_id = cmd
        .client_id
        .clone()
        .unwrap_or_else(|| format!("mqttv5-lat-{}", rand::rng().random::<u32>()));

    eprintln!("connecting to {} for latency test...", broker_url);

    let pub_client = MqttClient::new(format!("{base_client_id}-pub"));
    let pub_options = ConnectOptions::new(format!("{base_client_id}-pub"))
        .with_clean_start(true)
        .with_keep_alive(Duration::from_secs(30));

    pub_client
        .connect_with_options(&broker_url, pub_options)
        .await
        .context("failed to connect publisher")?;

    let sub_client = MqttClient::new(format!("{base_client_id}-sub"));
    let sub_options = ConnectOptions::new(format!("{base_client_id}-sub"))
        .with_clean_start(true)
        .with_keep_alive(Duration::from_secs(30));

    sub_client
        .connect_with_options(&broker_url, sub_options)
        .await
        .context("failed to connect subscriber")?;

    let latencies = Arc::new(Mutex::new(Vec::with_capacity(10000)));
    let latencies_clone = Arc::clone(&latencies);
    let topic = cmd.topic.clone();
    let filter = cmd.filter.clone().unwrap_or_else(|| topic.clone());

    sub_client
        .subscribe(&filter, move |msg| {
            let payload = &msg.payload;
            if payload.len() >= 8 {
                let sent_nanos = u64::from_be_bytes(payload[0..8].try_into().unwrap());
                let now_nanos = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos() as u64;
                let latency_us = (now_nanos.saturating_sub(sent_nanos)) / 1000;
                latencies_clone.lock().unwrap().push(latency_us);
            }
        })
        .await
        .context("failed to subscribe")?;

    eprintln!("warming up for {}s...", cmd.warmup);

    let message_rate = 1000;
    let interval_us = 1_000_000 / message_rate;
    let mut payload = vec![0u8; cmd.payload_size.max(8)];

    for _ in 0..(cmd.warmup * message_rate) {
        let now_nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        payload[0..8].copy_from_slice(&now_nanos.to_be_bytes());
        publish_message(&pub_client, &topic, &payload, cmd.qos).await?;
        tokio::time::sleep(Duration::from_micros(interval_us)).await;
    }

    {
        latencies.lock().unwrap().clear();
    }

    eprintln!(
        "measuring for {}s at {} msg/s...",
        cmd.duration, message_rate
    );
    let measure_start = Instant::now();
    let measure_duration = Duration::from_secs(cmd.duration);

    while measure_start.elapsed() < measure_duration {
        let now_nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64;
        payload[0..8].copy_from_slice(&now_nanos.to_be_bytes());
        publish_message(&pub_client, &topic, &payload, cmd.qos).await?;
        tokio::time::sleep(Duration::from_micros(interval_us)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut samples = latencies.lock().unwrap().clone();
    samples.sort_unstable();

    let (min_us, max_us, avg_us, p50_us, p95_us, p99_us) = if samples.is_empty() {
        (0, 0, 0.0, 0, 0, 0)
    } else {
        let min = samples[0];
        let max = samples[samples.len() - 1];
        let avg = samples.iter().sum::<u64>() as f64 / samples.len() as f64;
        let p50 = samples[samples.len() * 50 / 100];
        let p95 = samples[samples.len() * 95 / 100];
        let p99 = samples[samples.len() * 99 / 100];
        (min, max, avg, p50, p95, p99)
    };

    eprintln!(
        "  p50: {}us, p95: {}us, p99: {}us, min: {}us, max: {}us",
        p50_us, p95_us, p99_us, min_us, max_us
    );

    let output = BenchOutput {
        mode: "latency".to_string(),
        config: BenchConfig {
            duration_secs: cmd.duration,
            warmup_secs: cmd.warmup,
            payload_size: cmd.payload_size,
            qos: cmd.qos as u8,
            topic: cmd.topic,
            filter,
            publishers: 1,
            subscribers: 1,
        },
        results: BenchResults::Latency(LatencyResults {
            messages: samples.len() as u64,
            min_us,
            max_us,
            avg_us,
            p50_us,
            p95_us,
            p99_us,
            samples: samples
                .iter()
                .step_by(samples.len().max(1) / 100)
                .copied()
                .collect(),
        }),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);

    pub_client.disconnect().await.ok();
    sub_client.disconnect().await.ok();
    Ok(())
}

async fn run_connections(cmd: BenchCommand) -> Result<()> {
    use std::net::ToSocketAddrs;
    use std::sync::Mutex;

    let original_url = cmd
        .url
        .clone()
        .unwrap_or_else(|| format!("mqtt://{}:{}", cmd.host, cmd.port));

    let broker_url = if let Some(rest) = original_url.strip_prefix("mqtt://") {
        let addr_str = rest.split('/').next().unwrap_or(rest);
        let resolved: std::net::SocketAddr = addr_str
            .to_socket_addrs()
            .context("failed to resolve broker address")?
            .next()
            .context("no addresses resolved")?;
        format!("mqtt://{resolved}")
    } else {
        original_url.clone()
    };

    let base_client_id = cmd
        .client_id
        .clone()
        .unwrap_or_else(|| format!("mqttv5-conn-{}", rand::rng().random::<u32>()));

    eprintln!(
        "benchmarking connection rate to {} with {} concurrent workers for {}s...",
        original_url, cmd.concurrency, cmd.duration
    );
    eprintln!("  (resolved to {})", broker_url);

    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let successful = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let connect_times = Arc::new(Mutex::new(Vec::with_capacity(10000)));
    let counter = Arc::new(AtomicU64::new(0));

    let measure_duration = Duration::from_secs(cmd.duration);
    let measure_start = Instant::now();

    let mut handles = Vec::with_capacity(cmd.concurrency);
    for _ in 0..cmd.concurrency {
        let broker_url = broker_url.clone();
        let base_client_id = base_client_id.clone();
        let running = Arc::clone(&running);
        let successful = Arc::clone(&successful);
        let failed = Arc::clone(&failed);
        let connect_times = Arc::clone(&connect_times);
        let counter = Arc::clone(&counter);

        handles.push(tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                let id = counter.fetch_add(1, Ordering::Relaxed);
                let client_id = format!("{base_client_id}-{id}");
                let client = MqttClient::new(&client_id);
                let options = ConnectOptions::new(client_id)
                    .with_clean_start(true)
                    .with_keep_alive(Duration::from_secs(30));

                let start = Instant::now();
                match client.connect_with_options(&broker_url, options).await {
                    Ok(_) => {
                        let elapsed_us = start.elapsed().as_micros() as u64;
                        successful.fetch_add(1, Ordering::Relaxed);
                        connect_times.lock().unwrap().push(elapsed_us);
                        client.disconnect().await.ok();
                    }
                    Err(_) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }));
    }

    let mut samples: Vec<u64> = Vec::new();
    let mut last_count = 0u64;
    let mut next_sample = measure_start + Duration::from_secs(1);

    while Instant::now() < measure_start + measure_duration {
        tokio::time::sleep(Duration::from_millis(100)).await;

        if Instant::now() >= next_sample {
            let current = successful.load(Ordering::Relaxed);
            let delta = current - last_count;
            samples.push(delta);
            eprintln!("  {} conn/s", delta);
            last_count = current;
            next_sample += Duration::from_secs(1);
        }
    }

    running.store(false, Ordering::SeqCst);
    for handle in handles {
        handle.await.ok();
    }

    let total_successful = successful.load(Ordering::Relaxed);
    let total_failed = failed.load(Ordering::Relaxed);
    let elapsed = measure_start.elapsed().as_secs_f64();
    let connections_per_sec = total_successful as f64 / elapsed;

    let mut times = connect_times.lock().unwrap().clone();
    times.sort_unstable();

    let (avg_connect_us, p50_connect_us, p95_connect_us, p99_connect_us) = if times.is_empty() {
        (0.0, 0, 0, 0)
    } else {
        let avg = times.iter().sum::<u64>() as f64 / times.len() as f64;
        let p50 = times[times.len() * 50 / 100];
        let p95 = times[times.len() * 95 / 100];
        let p99 = times[times.len() * 99 / 100];
        (avg, p50, p95, p99)
    };

    eprintln!(
        "\n  total: {} successful, {} failed",
        total_successful, total_failed
    );
    eprintln!(
        "  avg: {:.0}us, p50: {}us, p95: {}us, p99: {}us",
        avg_connect_us, p50_connect_us, p95_connect_us, p99_connect_us
    );

    let output = BenchOutput {
        mode: "connections".to_string(),
        config: BenchConfig {
            duration_secs: cmd.duration,
            warmup_secs: 0,
            payload_size: 0,
            qos: 0,
            topic: String::new(),
            filter: String::new(),
            publishers: 0,
            subscribers: 0,
        },
        results: BenchResults::Connections(ConnectionResults {
            total_connections: total_successful + total_failed,
            successful: total_successful,
            failed: total_failed,
            elapsed_secs: elapsed,
            connections_per_sec,
            avg_connect_us,
            p50_connect_us,
            p95_connect_us,
            p99_connect_us,
            samples,
        }),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}
