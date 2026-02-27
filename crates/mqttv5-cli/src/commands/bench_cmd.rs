#![allow(clippy::struct_excessive_bools)]

use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use rand::Rng;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::time::Instant;

use super::parsers::{parse_duration_secs, parse_stream_strategy};

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum BenchMode {
    #[default]
    Throughput,
    Latency,
    Connections,
    HolBlocking,
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

    /// Full broker URL for TLS/WebSocket/QUIC (e.g., <mqtts://host:8883>, <wss://host/mqtt>)
    #[arg(long, short = 'U', conflicts_with_all = &["host", "port"])]
    pub url: Option<String>,

    /// Broker hostname (builds mqtt:// URL, use --url for TLS/WebSocket/QUIC)
    #[arg(long, short = 'H', default_value = "localhost")]
    pub host: String,

    /// Broker port (used with --host)
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

    #[arg(long)]
    pub insecure: bool,

    #[arg(long)]
    pub ca_cert: Option<PathBuf>,

    #[arg(long)]
    pub cert: Option<PathBuf>,

    #[arg(long)]
    pub key: Option<PathBuf>,

    #[arg(long, value_parser = parse_stream_strategy)]
    pub quic_stream_strategy: Option<mqtt5::transport::StreamStrategy>,

    #[arg(long)]
    pub quic_flow_headers: bool,

    #[arg(long, default_value = "300", value_parser = parse_duration_secs)]
    pub quic_flow_expire: u64,

    #[arg(long)]
    pub quic_max_streams: Option<usize>,

    #[arg(long)]
    pub quic_datagrams: bool,

    #[arg(long, default_value = "30", value_parser = parse_duration_secs)]
    pub quic_connect_timeout: u64,

    #[arg(long, default_value = "4")]
    pub topics: usize,

    #[arg(long, default_value = "0")]
    pub rate: u64,

    #[arg(long, conflicts_with = "rate")]
    pub load_factor: Option<f64>,

    #[arg(long, default_value = "5")]
    pub calibration_duration: u64,
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
struct CalibrationMetadata {
    calibrated_capacity: f64,
    target_rate: f64,
    load_factor: f64,
    calibration_secs: u64,
    steady_state_samples: Vec<u64>,
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
    transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    quic_stream_strategy: Option<String>,
    quic_datagrams: bool,
    quic_flow_headers: bool,
    rate: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    calibration: Option<CalibrationMetadata>,
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

#[derive(Clone)]
struct TimestampedSample {
    received_at_us: u64,
    latency_us: u64,
}

#[derive(Serialize)]
struct TopicLatencyResult {
    topic: String,
    messages: u64,
    rate: f64,
    p50_us: u64,
    p95_us: u64,
    p99_us: u64,
}

#[derive(Serialize)]
struct HolBlockingResults {
    topics: Vec<TopicLatencyResult>,
    windowed_correlation: f64,
    raw_correlation: f64,
    window_size_ms: u64,
    window_count: usize,
    total_messages: u64,
    measured_rate: f64,
}

#[derive(Serialize)]
#[serde(untagged)]
enum BenchResults {
    Throughput(ThroughputResults),
    Latency(LatencyResults),
    Connections(ConnectionResults),
    HolBlocking(HolBlockingResults),
}

#[derive(Serialize)]
struct BenchOutput {
    mode: String,
    config: BenchConfig,
    results: BenchResults,
}

pub async fn execute(cmd: BenchCommand, verbose: bool, debug: bool) -> Result<()> {
    crate::init_basic_tracing(verbose, debug);

    if cmd.load_factor.is_some() && !matches!(cmd.mode, BenchMode::HolBlocking | BenchMode::Latency)
    {
        anyhow::bail!("--load-factor is only supported with --mode hol-blocking or --mode latency");
    }

    match cmd.mode {
        BenchMode::Throughput => run_throughput(cmd).await,
        BenchMode::Latency => run_latency(cmd).await,
        BenchMode::Connections => run_connections(cmd).await,
        BenchMode::HolBlocking => run_hol_blocking(cmd).await,
    }
}

fn broker_url(cmd: &BenchCommand) -> String {
    cmd.url
        .clone()
        .unwrap_or_else(|| format!("mqtt://{}:{}", cmd.host, cmd.port))
}

fn transport_from_url(url: &str) -> String {
    url.split("://").next().unwrap_or("tcp").to_string()
}

fn strategy_display(s: mqtt5::transport::StreamStrategy) -> String {
    match s {
        mqtt5::transport::StreamStrategy::ControlOnly => "control-only".to_string(),
        mqtt5::transport::StreamStrategy::DataPerPublish => "per-publish".to_string(),
        mqtt5::transport::StreamStrategy::DataPerTopic => "per-topic".to_string(),
        mqtt5::transport::StreamStrategy::DataPerSubscription => "per-subscription".to_string(),
    }
}

fn bench_config(
    cmd: &BenchCommand,
    url: &str,
    calibration: Option<CalibrationMetadata>,
) -> BenchConfig {
    let filter = cmd.filter.clone().unwrap_or_else(|| cmd.topic.clone());
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rate = calibration
        .as_ref()
        .map_or(cmd.rate, |c| c.target_rate as u64);
    BenchConfig {
        duration_secs: cmd.duration,
        warmup_secs: cmd.warmup,
        payload_size: cmd.payload_size,
        qos: cmd.qos as u8,
        topic: cmd.topic.clone(),
        filter,
        publishers: cmd.publishers,
        subscribers: cmd.subscribers,
        transport: transport_from_url(url),
        quic_stream_strategy: cmd.quic_stream_strategy.map(strategy_display),
        quic_datagrams: cmd.quic_datagrams,
        quic_flow_headers: cmd.quic_flow_headers,
        rate,
        calibration,
    }
}

fn base_client_id(cmd: &BenchCommand, prefix: &str) -> String {
    cmd.client_id
        .clone()
        .unwrap_or_else(|| format!("mqttv5-{prefix}-{}", rand::rng().random::<u32>()))
}

async fn configure_transport(client: &MqttClient, cmd: &BenchCommand, url: &str) -> Result<()> {
    if cmd.insecure {
        client.set_insecure_tls(true).await;
    }
    if let Some(strategy) = cmd.quic_stream_strategy {
        client.set_quic_stream_strategy(strategy).await;
    }
    if cmd.quic_flow_headers {
        client.set_quic_flow_headers(true).await;
    }
    client
        .set_quic_flow_expire(std::time::Duration::from_secs(cmd.quic_flow_expire))
        .await;
    if let Some(max) = cmd.quic_max_streams {
        client.set_quic_max_streams(Some(max)).await;
    }
    if cmd.quic_datagrams {
        client.set_quic_datagrams(true).await;
    }
    client
        .set_quic_connect_timeout(Duration::from_secs(cmd.quic_connect_timeout))
        .await;

    let is_secure =
        url.starts_with("ssl://") || url.starts_with("mqtts://") || url.starts_with("quics://");
    let has_certs = cmd.cert.is_some() || cmd.key.is_some() || cmd.ca_cert.is_some();
    if is_secure && has_certs {
        let cert_pem = if let Some(p) = &cmd.cert {
            Some(
                std::fs::read(p)
                    .with_context(|| format!("failed to read cert: {}", p.display()))?,
            )
        } else {
            None
        };
        let key_pem = if let Some(p) = &cmd.key {
            Some(std::fs::read(p).with_context(|| format!("failed to read key: {}", p.display()))?)
        } else {
            None
        };
        let ca_pem = if let Some(p) = &cmd.ca_cert {
            Some(
                std::fs::read(p)
                    .with_context(|| format!("failed to read CA cert: {}", p.display()))?,
            )
        } else {
            None
        };
        client.set_tls_config(cert_pem, key_pem, ca_pem).await;
    }
    Ok(())
}

async fn connect_client(client_id: String, url: &str, cmd: &BenchCommand) -> Result<MqttClient> {
    let client = MqttClient::new(&client_id);
    configure_transport(&client, cmd, url).await?;
    let options = ConnectOptions::new(client_id)
        .with_clean_start(true)
        .with_keep_alive(Duration::from_secs(30));
    client
        .connect_with_options(url, options)
        .await
        .context("failed to connect")?;
    Ok(client)
}

fn as_f64_lossy(value: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let result = value as f64;
    result
}

fn usize_as_f64_lossy(value: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let result = value as f64;
    result
}

fn nanos_as_u64() -> u64 {
    #[allow(clippy::cast_possible_truncation)]
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64;
    nanos
}

fn micros_as_u64(duration: std::time::Duration) -> u64 {
    #[allow(clippy::cast_possible_truncation)]
    let micros = duration.as_micros() as u64;
    micros
}

fn percentile_stats(sorted: &[u64]) -> (f64, u64, u64, u64) {
    if sorted.is_empty() {
        return (0.0, 0, 0, 0);
    }
    let avg = as_f64_lossy(sorted.iter().sum::<u64>()) / usize_as_f64_lossy(sorted.len());
    let p50 = sorted[sorted.len() * 50 / 100];
    let p95 = sorted[sorted.len() * 95 / 100];
    let p99 = sorted[sorted.len() * 99 / 100];
    (avg, p50, p95, p99)
}

fn spawn_publishers(
    pub_clients: Vec<MqttClient>,
    topic_base: &str,
    payload: &Arc<[u8]>,
    qos: QoS,
    running: &Arc<std::sync::atomic::AtomicBool>,
    published: &Arc<AtomicU64>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::with_capacity(pub_clients.len());
    for (i, pub_client) in pub_clients.into_iter().enumerate() {
        let topic = format!("{topic_base}/{i}");
        let payload = Arc::clone(payload);
        let running = Arc::clone(running);
        let published = Arc::clone(published);

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
    handles
}

async fn run_throughput(cmd: BenchCommand) -> Result<()> {
    let url = broker_url(&cmd);
    let base_id = base_client_id(&cmd, "bench");

    eprintln!(
        "connecting {} publisher(s) and {} subscriber(s) to {url}...",
        cmd.publishers, cmd.subscribers
    );

    let mut pub_clients = Vec::with_capacity(cmd.publishers);
    for i in 0..cmd.publishers {
        pub_clients.push(connect_client(format!("{base_id}-pub-{i}"), &url, &cmd).await?);
    }

    let received = Arc::new(AtomicU64::new(0));
    let topic = cmd.topic.clone();
    let filter = cmd.filter.clone().unwrap_or_else(|| format!("{topic}/#"));

    let mut sub_clients = Vec::with_capacity(cmd.subscribers);
    for i in 0..cmd.subscribers {
        let sub_client = connect_client(format!("{base_id}-sub-{i}"), &url, &cmd).await?;
        let received_clone = Arc::clone(&received);
        sub_client
            .subscribe(&filter, move |_| {
                received_clone.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .context("failed to subscribe")?;
        sub_clients.push(sub_client);
    }

    eprintln!("subscribed {} client(s) to {filter}", cmd.subscribers);

    let payload: Arc<[u8]> = vec![0u8; cmd.payload_size].into();
    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let published = Arc::new(AtomicU64::new(0));

    eprintln!("warming up for {}s...", cmd.warmup);
    let handles = spawn_publishers(pub_clients, &topic, &payload, cmd.qos, &running, &published);

    tokio::time::sleep(Duration::from_secs(cmd.warmup)).await;
    received.store(0, Ordering::SeqCst);
    published.store(0, Ordering::SeqCst);

    eprintln!("measuring for {}s...", cmd.duration);
    let measure_start = Instant::now();
    let samples =
        sample_counter_per_second(measure_start, Duration::from_secs(cmd.duration), &received)
            .await;

    running.store(false, Ordering::SeqCst);
    for handle in handles {
        handle.await.ok();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;

    let total_published = published.load(Ordering::Relaxed);
    let total_received = received.load(Ordering::Relaxed);
    let elapsed = measure_start.elapsed().as_secs_f64();
    let throughput_avg = as_f64_lossy(total_received) / elapsed;

    let output = BenchOutput {
        mode: "throughput".to_string(),
        config: bench_config(&cmd, &url, None),
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

async fn sample_counter_per_second(
    start: Instant,
    duration: Duration,
    counter: &AtomicU64,
) -> Vec<u64> {
    let end = start + duration;
    let mut next_sample = start + Duration::from_secs(1);
    let mut last_count = 0u64;
    let mut samples = Vec::new();

    while Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(10)).await;
        if Instant::now() >= next_sample {
            let current = counter.load(Ordering::Relaxed);
            let delta = current - last_count;
            samples.push(delta);
            eprintln!("  {delta} msg/s");
            last_count = current;
            next_sample += Duration::from_secs(1);
        }
    }
    samples
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

    let url = broker_url(&cmd);
    let base_id = base_client_id(&cmd, "lat");

    eprintln!("connecting to {url} for latency test...");

    let pub_client = connect_client(format!("{base_id}-pub"), &url, &cmd).await?;
    let sub_client = connect_client(format!("{base_id}-sub"), &url, &cmd).await?;

    let latencies = Arc::new(Mutex::new(Vec::with_capacity(10000)));
    let latencies_clone = Arc::clone(&latencies);
    let topic = cmd.topic.clone();
    let filter = cmd.filter.clone().unwrap_or_else(|| topic.clone());

    sub_client
        .subscribe(&filter, move |msg| {
            let payload = &msg.payload;
            if payload.len() >= 8 {
                let sent_nanos = u64::from_be_bytes(payload[0..8].try_into().unwrap());
                let now_nanos = nanos_as_u64();
                let latency_us = (now_nanos.saturating_sub(sent_nanos)) / 1000;
                latencies_clone.lock().unwrap().push(latency_us);
            }
        })
        .await
        .context("failed to subscribe")?;

    let payload_size = cmd.payload_size.max(8);
    let (rate_interval_us, calibration_meta) =
        determine_publish_rate(&cmd, &url, &base_id, 1, payload_size).await?;
    let interval_us = rate_interval_us.unwrap_or(1000);
    let message_rate = 1_000_000 / interval_us;

    let rate_label = if let Some(ref meta) = calibration_meta {
        format!("{:.0} msg/s (calibrated)", meta.target_rate)
    } else {
        format!("{message_rate} msg/s")
    };

    let mut payload = vec![0u8; payload_size];

    eprintln!("warming up for {}s at {rate_label}...", cmd.warmup);
    send_timed_messages(
        &pub_client,
        &topic,
        &mut payload,
        cmd.qos,
        cmd.warmup * message_rate,
        interval_us,
    )
    .await?;
    latencies.lock().unwrap().clear();

    eprintln!("measuring for {}s at {rate_label}...", cmd.duration);
    let measure_start = Instant::now();
    let measure_duration = Duration::from_secs(cmd.duration);
    while measure_start.elapsed() < measure_duration {
        payload[0..8].copy_from_slice(&nanos_as_u64().to_be_bytes());
        publish_message(&pub_client, &topic, &payload, cmd.qos).await?;
        tokio::time::sleep(Duration::from_micros(interval_us)).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut samples = latencies.lock().unwrap().clone();
    samples.sort_unstable();

    let (min_us, max_us) = if samples.is_empty() {
        (0, 0)
    } else {
        (samples[0], samples[samples.len() - 1])
    };
    let (avg_us, p50_us, p95_us, p99_us) = percentile_stats(&samples);

    eprintln!(
        "  p50: {p50_us}us, p95: {p95_us}us, p99: {p99_us}us, min: {min_us}us, max: {max_us}us"
    );

    let output = BenchOutput {
        mode: "latency".to_string(),
        config: bench_config(&cmd, &url, calibration_meta),
        results: BenchResults::Latency(LatencyResults {
            messages: samples.len() as u64,
            min_us,
            max_us,
            avg_us,
            p50_us,
            p95_us,
            p99_us,
            samples: downsample(&samples, 100),
        }),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);

    pub_client.disconnect().await.ok();
    sub_client.disconnect().await.ok();
    Ok(())
}

fn downsample(sorted: &[u64], target: usize) -> Vec<u64> {
    if sorted.len() <= target {
        return sorted.to_vec();
    }
    sorted
        .iter()
        .step_by(sorted.len() / target)
        .copied()
        .collect()
}

async fn send_timed_messages(
    client: &MqttClient,
    topic: &str,
    payload: &mut [u8],
    qos: QoS,
    count: u64,
    interval_us: u64,
) -> Result<()> {
    for _ in 0..count {
        payload[0..8].copy_from_slice(&nanos_as_u64().to_be_bytes());
        publish_message(client, topic, payload, qos).await?;
        tokio::time::sleep(Duration::from_micros(interval_us)).await;
    }
    Ok(())
}

fn load_tls_certs(cmd: &BenchCommand) -> Result<TlsCerts> {
    let cert_pem = cmd
        .cert
        .as_ref()
        .map(std::fs::read)
        .transpose()
        .context("failed to read cert")?
        .map(Arc::new);
    let key_pem = cmd
        .key
        .as_ref()
        .map(std::fs::read)
        .transpose()
        .context("failed to read key")?
        .map(Arc::new);
    let ca_pem = cmd
        .ca_cert
        .as_ref()
        .map(std::fs::read)
        .transpose()
        .context("failed to read CA cert")?
        .map(Arc::new);
    Ok(TlsCerts {
        cert: cert_pem,
        key: key_pem,
        ca: ca_pem,
    })
}

struct TlsCerts {
    cert: Option<Arc<Vec<u8>>>,
    key: Option<Arc<Vec<u8>>>,
    ca: Option<Arc<Vec<u8>>>,
}

async fn run_connections(cmd: BenchCommand) -> Result<()> {
    use std::sync::Mutex;

    let original_url = broker_url(&cmd);
    let resolved_url = resolve_broker_url(&original_url)?;
    let base_id = base_client_id(&cmd, "conn");

    eprintln!(
        "benchmarking connection rate to {original_url} with {} concurrent workers for {}s...",
        cmd.concurrency, cmd.duration
    );
    eprintln!("  (resolved to {resolved_url})");

    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let successful = Arc::new(AtomicU64::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let connect_times = Arc::new(Mutex::new(Vec::with_capacity(10000)));
    let counter = Arc::new(AtomicU64::new(0));

    let measure_start = Instant::now();
    let measure_duration = Duration::from_secs(cmd.duration);

    let tls = load_tls_certs(&cmd)?;
    let state = ConnectionBenchState {
        broker_url: resolved_url,
        base_client_id: base_id,
        insecure: cmd.insecure,
        quic_stream_strategy: cmd.quic_stream_strategy,
        quic_flow_headers: cmd.quic_flow_headers,
        quic_flow_expire: cmd.quic_flow_expire,
        quic_max_streams: cmd.quic_max_streams,
        quic_datagrams: cmd.quic_datagrams,
        quic_connect_timeout: cmd.quic_connect_timeout,
        cert_pem: tls.cert,
        key_pem: tls.key,
        ca_pem: tls.ca,
        running: Arc::clone(&running),
        successful: Arc::clone(&successful),
        failed: Arc::clone(&failed),
        connect_times: Arc::clone(&connect_times),
        counter: Arc::clone(&counter),
    };
    let handles = spawn_connection_workers(cmd.concurrency, &state);

    let samples = sample_counter_per_second(measure_start, measure_duration, &successful).await;

    running.store(false, Ordering::SeqCst);
    for handle in handles {
        handle.await.ok();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;

    let total_successful = successful.load(Ordering::Relaxed);
    let total_failed = failed.load(Ordering::Relaxed);
    let elapsed = measure_start.elapsed().as_secs_f64();
    let connections_per_sec = as_f64_lossy(total_successful) / elapsed;

    let mut times = connect_times.lock().unwrap().clone();
    times.sort_unstable();

    let (avg_connect_us, p50_connect_us, p95_connect_us, p99_connect_us) = percentile_stats(&times);

    eprintln!("\n  total: {total_successful} successful, {total_failed} failed");
    eprintln!("  avg: {avg_connect_us:.0}us, p50: {p50_connect_us}us, p95: {p95_connect_us}us, p99: {p99_connect_us}us");

    let output = BenchOutput {
        mode: "connections".to_string(),
        config: {
            let mut cfg = bench_config(&cmd, &original_url, None);
            cfg.warmup_secs = 0;
            cfg.payload_size = 0;
            cfg.qos = 0;
            cfg.topic = String::new();
            cfg.filter = String::new();
            cfg.publishers = 0;
            cfg.subscribers = 0;
            cfg
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

fn resolve_broker_url(original_url: &str) -> Result<String> {
    use std::net::ToSocketAddrs;

    if let Some(rest) = original_url.strip_prefix("mqtt://") {
        let addr_str = rest.split('/').next().unwrap_or(rest);
        let resolved: std::net::SocketAddr = addr_str
            .to_socket_addrs()
            .context("failed to resolve broker address")?
            .next()
            .context("no addresses resolved")?;
        Ok(format!("mqtt://{resolved}"))
    } else {
        Ok(original_url.to_string())
    }
}

struct ConnectionBenchState {
    broker_url: String,
    base_client_id: String,
    insecure: bool,
    quic_stream_strategy: Option<mqtt5::transport::StreamStrategy>,
    quic_flow_headers: bool,
    quic_flow_expire: u64,
    quic_max_streams: Option<usize>,
    quic_datagrams: bool,
    quic_connect_timeout: u64,
    cert_pem: Option<Arc<Vec<u8>>>,
    key_pem: Option<Arc<Vec<u8>>>,
    ca_pem: Option<Arc<Vec<u8>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
    successful: Arc<AtomicU64>,
    failed: Arc<AtomicU64>,
    connect_times: Arc<std::sync::Mutex<Vec<u64>>>,
    counter: Arc<AtomicU64>,
}

fn spawn_connection_workers(
    concurrency: usize,
    state: &ConnectionBenchState,
) -> Vec<tokio::task::JoinHandle<()>> {
    let is_secure = state.broker_url.starts_with("ssl://")
        || state.broker_url.starts_with("mqtts://")
        || state.broker_url.starts_with("quics://");
    let has_certs = state.cert_pem.is_some() || state.key_pem.is_some() || state.ca_pem.is_some();

    let mut handles = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        let broker_url = state.broker_url.clone();
        let base_client_id = state.base_client_id.clone();
        let insecure = state.insecure;
        let quic_stream_strategy = state.quic_stream_strategy;
        let quic_flow_headers = state.quic_flow_headers;
        let quic_flow_expire = state.quic_flow_expire;
        let quic_max_streams = state.quic_max_streams;
        let quic_datagrams = state.quic_datagrams;
        let quic_connect_timeout = state.quic_connect_timeout;
        let cert_pem = state.cert_pem.clone();
        let key_pem = state.key_pem.clone();
        let ca_pem = state.ca_pem.clone();
        let configure_tls = is_secure && has_certs;
        let running = Arc::clone(&state.running);
        let successful = Arc::clone(&state.successful);
        let failed = Arc::clone(&state.failed);
        let connect_times = Arc::clone(&state.connect_times);
        let counter = Arc::clone(&state.counter);

        handles.push(tokio::spawn(async move {
            while running.load(Ordering::Relaxed) {
                let id = counter.fetch_add(1, Ordering::Relaxed);
                let client_id = format!("{base_client_id}-{id}");
                let client = MqttClient::new(&client_id);

                if insecure {
                    client.set_insecure_tls(true).await;
                }
                if configure_tls {
                    client
                        .set_tls_config(
                            cert_pem.as_deref().cloned(),
                            key_pem.as_deref().cloned(),
                            ca_pem.as_deref().cloned(),
                        )
                        .await;
                }
                if let Some(strategy) = quic_stream_strategy {
                    client.set_quic_stream_strategy(strategy).await;
                }
                if quic_flow_headers {
                    client.set_quic_flow_headers(true).await;
                }
                client
                    .set_quic_flow_expire(std::time::Duration::from_secs(quic_flow_expire))
                    .await;
                if let Some(max) = quic_max_streams {
                    client.set_quic_max_streams(Some(max)).await;
                }
                if quic_datagrams {
                    client.set_quic_datagrams(true).await;
                }
                client
                    .set_quic_connect_timeout(Duration::from_secs(quic_connect_timeout))
                    .await;

                let options = ConnectOptions::new(client_id)
                    .with_clean_start(true)
                    .with_keep_alive(Duration::from_secs(30));

                let start = Instant::now();
                match client.connect_with_options(&broker_url, options).await {
                    Ok(_) => {
                        let elapsed_us = micros_as_u64(start.elapsed());
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
    handles
}

async fn calibrate_capacity(
    url: &str,
    cmd: &BenchCommand,
    base_id: &str,
    num_topics: usize,
    payload_size: usize,
    duration_secs: u64,
) -> Result<(f64, Vec<u64>)> {
    use std::sync::Mutex;

    let publisher = connect_client(format!("{base_id}-cal-pub"), url, cmd).await?;
    let subscriber = connect_client(format!("{base_id}-cal-sub"), url, cmd).await?;

    let received = Arc::new(AtomicU64::new(0));
    for i in 0..num_topics {
        let counter = Arc::clone(&received);
        let topic_filter = format!("bench/hol/{i}");
        subscriber
            .subscribe(&topic_filter, move |_msg| {
                counter.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .context("calibration subscribe failed")?;
    }

    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let sent_count = Arc::new(AtomicU64::new(0));
    let handles = spawn_hol_publishers(
        &publisher,
        num_topics,
        payload_size,
        None,
        &running,
        &sent_count,
    );

    #[allow(clippy::cast_possible_truncation)]
    let per_second_samples = Arc::new(Mutex::new(Vec::with_capacity(duration_secs as usize)));
    let samples_clone = Arc::clone(&per_second_samples);
    let received_sampler = Arc::clone(&received);
    let sampler_handle = tokio::spawn(async move {
        for _ in 0..duration_secs {
            let before = received_sampler.load(Ordering::Relaxed);
            tokio::time::sleep(Duration::from_secs(1)).await;
            let after = received_sampler.load(Ordering::Relaxed);
            samples_clone.lock().unwrap().push(after - before);
        }
    });

    sampler_handle.await?;
    running.store(false, Ordering::Relaxed);
    for h in handles {
        h.await.ok();
    }

    publisher.disconnect().await.ok();
    subscriber.disconnect().await.ok();

    let samples = per_second_samples.lock().unwrap().clone();
    let ramp_up = 2usize.min(samples.len().saturating_sub(1));
    let steady: &[u64] = &samples[ramp_up..];

    anyhow::ensure!(
        !steady.is_empty(),
        "calibration produced no steady-state samples"
    );

    #[allow(clippy::cast_precision_loss)]
    let capacity = steady.iter().sum::<u64>() as f64 / steady.len() as f64;

    anyhow::ensure!(
        capacity >= 100.0,
        "calibration failed: capacity {capacity:.0} msg/s is below minimum threshold"
    );

    #[allow(clippy::cast_precision_loss)]
    let cv = if capacity > 0.0 {
        let mean = capacity;
        let count = steady.len() as f64;
        let variance = steady
            .iter()
            .map(|&s| (s as f64 - mean).powi(2))
            .sum::<f64>()
            / count;
        variance.sqrt() / mean
    } else {
        0.0
    };
    if cv > 0.3 {
        eprintln!("warning: calibration variance is high (CV={cv:.2}) -- capacity estimate may be unreliable");
    }

    Ok((capacity, samples))
}

async fn subscribe_hol_topics(
    sub_client: &MqttClient,
    num_topics: usize,
    topic_samples: &[Arc<std::sync::Mutex<Vec<TimestampedSample>>>],
    measure_start_nanos: &Arc<AtomicU64>,
) -> Result<()> {
    for (i, samples_vec) in topic_samples.iter().enumerate() {
        let topic_filter = format!("bench/hol/{i}");
        let samples_clone = Arc::clone(samples_vec);
        let start_nanos = Arc::clone(measure_start_nanos);
        sub_client
            .subscribe(&topic_filter, move |msg| {
                let payload = &msg.payload;
                if payload.len() >= 8 {
                    let sent_nanos = u64::from_be_bytes(payload[0..8].try_into().unwrap());
                    let now_nanos = nanos_as_u64();
                    let latency_us = (now_nanos.saturating_sub(sent_nanos)) / 1000;
                    let base = start_nanos.load(Ordering::Relaxed);
                    let received_at_us = if base > 0 {
                        (now_nanos.saturating_sub(base)) / 1000
                    } else {
                        0
                    };
                    samples_clone.lock().unwrap().push(TimestampedSample {
                        received_at_us,
                        latency_us,
                    });
                }
            })
            .await
            .context("failed to subscribe")?;
    }
    eprintln!("subscribed to {num_topics} topics");
    Ok(())
}

async fn determine_publish_rate(
    cmd: &BenchCommand,
    url: &str,
    base_id: &str,
    num_topics: usize,
    payload_size: usize,
) -> Result<(Option<u64>, Option<CalibrationMetadata>)> {
    if let Some(lf) = cmd.load_factor {
        anyhow::ensure!(lf > 0.0, "--load-factor must be positive");
        anyhow::ensure!(lf <= 2.0, "--load-factor must be <= 2.0");
        if lf > 1.0 {
            eprintln!("warning: load_factor {lf} exceeds 1.0 -- transport will be overloaded");
        }

        eprintln!("calibrating capacity for {}s...", cmd.calibration_duration);
        let (capacity, cal_samples) = calibrate_capacity(
            url,
            cmd,
            base_id,
            num_topics,
            payload_size,
            cmd.calibration_duration,
        )
        .await?;

        let target_rate = capacity * lf;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let interval = 1_000_000u64 * (num_topics as u64) / (target_rate as u64);

        eprintln!(
            "  capacity: {capacity:.0} msg/s, target: {target_rate:.0} msg/s (load_factor={lf})"
        );

        let meta = CalibrationMetadata {
            calibrated_capacity: capacity,
            target_rate,
            load_factor: lf,
            calibration_secs: cmd.calibration_duration,
            steady_state_samples: cal_samples,
        };
        Ok((Some(interval), Some(meta)))
    } else if cmd.rate > 0 {
        #[allow(clippy::cast_possible_truncation)]
        let interval = 1_000_000u64 * (num_topics as u64) / cmd.rate;
        Ok((Some(interval), None))
    } else {
        Ok((None, None))
    }
}

async fn run_hol_blocking(cmd: BenchCommand) -> Result<()> {
    use std::sync::Mutex;

    let url = broker_url(&cmd);
    let base_id = base_client_id(&cmd, "hol");
    let num_topics = cmd.topics;
    let payload_size = cmd.payload_size.max(8);

    eprintln!("connecting to {url} for HOL blocking test with {num_topics} topics...");

    let pub_client = connect_client(format!("{base_id}-pub"), &url, &cmd).await?;
    let sub_client = connect_client(format!("{base_id}-sub"), &url, &cmd).await?;

    let topic_samples: Vec<Arc<Mutex<Vec<TimestampedSample>>>> = (0..num_topics)
        .map(|_| Arc::new(Mutex::new(Vec::with_capacity(100_000))))
        .collect();

    let measure_start_nanos = Arc::new(AtomicU64::new(0));
    subscribe_hol_topics(
        &sub_client,
        num_topics,
        &topic_samples,
        &measure_start_nanos,
    )
    .await?;

    let (per_topic_interval_us, calibration_meta) =
        determine_publish_rate(&cmd, &url, &base_id, num_topics, payload_size).await?;

    let rate_label = if let Some(ref meta) = calibration_meta {
        format!("{:.0} msg/s (calibrated)", meta.target_rate)
    } else if cmd.rate > 0 {
        format!("{} msg/s", cmd.rate)
    } else {
        "unlimited".to_string()
    };

    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let published = Arc::new(AtomicU64::new(0));

    eprintln!("warming up for {}s at {rate_label}...", cmd.warmup);
    let warmup_handles = spawn_hol_publishers(
        &pub_client,
        num_topics,
        payload_size,
        per_topic_interval_us,
        &running,
        &published,
    );
    tokio::time::sleep(Duration::from_secs(cmd.warmup)).await;
    running.store(false, Ordering::SeqCst);
    for handle in warmup_handles {
        handle.await.ok();
    }

    for sv in &topic_samples {
        sv.lock().unwrap().clear();
    }
    published.store(0, Ordering::SeqCst);

    eprintln!("measuring for {}s at {rate_label}...", cmd.duration);
    running.store(true, Ordering::SeqCst);
    measure_start_nanos.store(nanos_as_u64(), Ordering::SeqCst);
    let measure_wall = Instant::now();

    let measure_handles = spawn_hol_publishers(
        &pub_client,
        num_topics,
        payload_size,
        per_topic_interval_us,
        &running,
        &published,
    );
    tokio::time::sleep(Duration::from_secs(cmd.duration)).await;
    running.store(false, Ordering::SeqCst);
    for handle in measure_handles {
        handle.await.ok();
    }

    let elapsed = measure_wall.elapsed().as_secs_f64();
    let total_published = published.load(Ordering::Relaxed);

    tokio::time::sleep(Duration::from_millis(500)).await;

    let results = gather_hol_results(&topic_samples, elapsed);

    eprintln!(
        "  published: {total_published}, received: {}, rate: {:.0} msg/s",
        results.total_messages, results.measured_rate
    );
    eprintln!(
        "  raw_correlation: {:.4}, windowed_correlation: {:.4} ({} windows of {}ms)",
        results.raw_correlation,
        results.windowed_correlation,
        results.window_count,
        results.window_size_ms
    );

    let output = BenchOutput {
        mode: "hol-blocking".to_string(),
        config: bench_config(&cmd, &url, calibration_meta),
        results: BenchResults::HolBlocking(results),
    };

    println!("{}", serde_json::to_string_pretty(&output)?);

    pub_client.disconnect().await.ok();
    sub_client.disconnect().await.ok();
    Ok(())
}

fn spawn_hol_publishers(
    pub_client: &MqttClient,
    num_topics: usize,
    payload_size: usize,
    per_topic_interval_us: Option<u64>,
    running: &Arc<std::sync::atomic::AtomicBool>,
    published: &Arc<AtomicU64>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::with_capacity(num_topics);
    for topic_idx in 0..num_topics {
        let client = pub_client.clone();
        let running = Arc::clone(running);
        let published = Arc::clone(published);

        handles.push(tokio::spawn(async move {
            let mut payload = vec![0u8; payload_size];
            let topic = format!("bench/hol/{topic_idx}");
            while running.load(Ordering::Relaxed) {
                payload[0..8].copy_from_slice(&nanos_as_u64().to_be_bytes());
                if client.publish(&topic, payload.clone()).await.is_ok() {
                    published.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(interval) = per_topic_interval_us {
                    tokio::time::sleep(Duration::from_micros(interval)).await;
                }
            }
        }));
    }
    handles
}

fn gather_hol_results(
    topic_samples: &[Arc<std::sync::Mutex<Vec<TimestampedSample>>>],
    elapsed_secs: f64,
) -> HolBlockingResults {
    let mut topic_results = Vec::with_capacity(topic_samples.len());
    let mut raw_latency_vecs: Vec<Vec<u64>> = Vec::with_capacity(topic_samples.len());
    let mut all_samples: Vec<Vec<TimestampedSample>> = Vec::with_capacity(topic_samples.len());
    let mut total_messages: u64 = 0;

    for (i, sv) in topic_samples.iter().enumerate() {
        let samples = sv.lock().unwrap().clone();
        let mut sorted_latencies: Vec<u64> = samples.iter().map(|s| s.latency_us).collect();
        let raw_latencies: Vec<u64> = sorted_latencies.clone();
        sorted_latencies.sort_unstable();

        let (_, p50, p95, p99) = percentile_stats(&sorted_latencies);
        #[allow(clippy::cast_precision_loss)]
        let msg_count = samples.len() as u64;
        let topic_rate = as_f64_lossy(msg_count) / elapsed_secs;

        eprintln!(
            "  topic bench/hol/{i}: {msg_count} msgs, {topic_rate:.0} msg/s, p50={p50}us, p95={p95}us, p99={p99}us",
        );
        topic_results.push(TopicLatencyResult {
            topic: format!("bench/hol/{i}"),
            messages: msg_count,
            rate: topic_rate,
            p50_us: p50,
            p95_us: p95,
            p99_us: p99,
        });

        total_messages += msg_count;
        raw_latency_vecs.push(raw_latencies);
        all_samples.push(samples);
    }

    let raw_correlation = pearson_correlation(&raw_latency_vecs);
    let window_size_ms = 500;
    let (windowed_corr, window_count) = windowed_correlation(&all_samples, window_size_ms);
    let measured_rate = as_f64_lossy(total_messages) / elapsed_secs;

    HolBlockingResults {
        topics: topic_results,
        windowed_correlation: windowed_corr,
        raw_correlation,
        window_size_ms,
        window_count,
        total_messages,
        measured_rate,
    }
}

fn pearson_correlation(topic_latencies: &[Vec<u64>]) -> f64 {
    if topic_latencies.len() < 2 {
        return 0.0;
    }

    let min_len = topic_latencies.iter().map(Vec::len).min().unwrap_or(0);
    if min_len < 2 {
        return 0.0;
    }

    let mut total_r = 0.0;
    let mut pair_count: u64 = 0;

    for i in 0..topic_latencies.len() {
        for j in (i + 1)..topic_latencies.len() {
            let r = pearson_pair(
                &topic_latencies[i][..min_len],
                &topic_latencies[j][..min_len],
            );
            if r.is_finite() {
                total_r += r;
                pair_count += 1;
            }
        }
    }

    if pair_count == 0 {
        return 0.0;
    }
    total_r / as_f64_lossy(pair_count)
}

fn pearson_pair(xs: &[u64], ys: &[u64]) -> f64 {
    let xf: Vec<f64> = xs.iter().map(|&v| as_f64_lossy(v)).collect();
    let yf: Vec<f64> = ys.iter().map(|&v| as_f64_lossy(v)).collect();
    pearson_pair_f64(&xf, &yf)
}

fn pearson_pair_f64(xs: &[f64], ys: &[f64]) -> f64 {
    let n = usize_as_f64_lossy(xs.len());
    let sum_first: f64 = xs.iter().sum();
    let sum_second: f64 = ys.iter().sum();
    let sum_product: f64 = xs.iter().zip(ys.iter()).map(|(x, y)| x * y).sum();
    let sum_first_sq: f64 = xs.iter().map(|v| v.powi(2)).sum();
    let sum_second_sq: f64 = ys.iter().map(|v| v.powi(2)).sum();

    let numerator = n.mul_add(sum_product, -(sum_first * sum_second));
    let denominator = (n.mul_add(sum_first_sq, -sum_first.powi(2))
        * n.mul_add(sum_second_sq, -sum_second.powi(2)))
    .sqrt();

    if denominator == 0.0 {
        return 0.0;
    }
    numerator / denominator
}

fn windowed_correlation(topic_samples: &[Vec<TimestampedSample>], window_ms: u64) -> (f64, usize) {
    if topic_samples.len() < 2 {
        return (0.0, 0);
    }

    let max_time_us = topic_samples
        .iter()
        .flat_map(|s| s.iter().map(|ts| ts.received_at_us))
        .max()
        .unwrap_or(0);

    let bucket_us = window_ms * 1000;
    if bucket_us == 0 || max_time_us == 0 {
        return (0.0, 0);
    }

    #[allow(clippy::cast_possible_truncation)]
    let num_windows = max_time_us.div_ceil(bucket_us) as usize;

    let mut per_topic_means: Vec<Vec<f64>> =
        vec![Vec::with_capacity(num_windows); topic_samples.len()];

    let mut valid_windows = 0usize;
    for w in 0..num_windows {
        let window_start = w as u64 * bucket_us;
        let window_end = window_start + bucket_us;

        let mut all_have_data = true;
        let mut window_means = Vec::with_capacity(topic_samples.len());
        for samples in topic_samples {
            let (sum, count) = samples
                .iter()
                .filter(|s| s.received_at_us >= window_start && s.received_at_us < window_end)
                .fold((0.0f64, 0u64), |(s, c), ts| {
                    (s + as_f64_lossy(ts.latency_us), c + 1)
                });
            if count == 0 {
                all_have_data = false;
                break;
            }
            window_means.push(sum / as_f64_lossy(count));
        }

        if all_have_data {
            for (i, mean) in window_means.into_iter().enumerate() {
                per_topic_means[i].push(mean);
            }
            valid_windows += 1;
        }
    }

    if valid_windows < 2 {
        return (0.0, valid_windows);
    }

    let mut total_r = 0.0;
    let mut pair_count = 0u64;
    for i in 0..per_topic_means.len() {
        for j in (i + 1)..per_topic_means.len() {
            let r = pearson_pair_f64(&per_topic_means[i], &per_topic_means[j]);
            if r.is_finite() {
                total_r += r;
                pair_count += 1;
            }
        }
    }

    if pair_count == 0 {
        return (0.0, valid_windows);
    }
    (total_r / as_f64_lossy(pair_count), valid_windows)
}
