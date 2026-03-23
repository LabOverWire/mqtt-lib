#![allow(clippy::struct_excessive_bools)]

use anyhow::{Context, Result};
use bebytes::BeBytes;
use clap::{Args, ValueEnum};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use mqtt5::time::Duration;
use mqtt5::{ConnectOptions, MqttClient, QoS};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::io::{Read as _, Write as _};
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

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
pub enum PayloadFormat {
    #[default]
    Raw,
    Json,
    Bebytes,
    CompressedJson,
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

    #[arg(long)]
    pub quic_early_data: bool,

    #[arg(
        long,
        help = "Separate URL for publishers in HOL mode (e.g., mqtt://host:1883)"
    )]
    pub pub_url: Option<String>,

    #[arg(long, default_value = "4")]
    pub topics: usize,

    #[arg(long, default_value = "0")]
    pub rate: u64,

    #[arg(long, value_enum, default_value = "raw")]
    pub payload_format: PayloadFormat,

    #[arg(long)]
    pub trace_dir: Option<PathBuf>,
}

fn parse_qos(s: &str) -> Result<QoS, String> {
    match s {
        "0" => Ok(QoS::AtMostOnce),
        "1" => Ok(QoS::AtLeastOnce),
        "2" => Ok(QoS::ExactlyOnce),
        _ => Err(format!("QoS must be 0, 1, or 2, got: {s}")),
    }
}

fn format_name(fmt: PayloadFormat) -> String {
    match fmt {
        PayloadFormat::Raw => "raw",
        PayloadFormat::Json => "json",
        PayloadFormat::Bebytes => "bebytes",
        PayloadFormat::CompressedJson => "compressed-json",
    }
    .to_string()
}

#[derive(Serialize, Deserialize)]
struct JsonPayload {
    ts: u64,
    seq: u32,
    dev: u32,
    readings: Vec<f64>,
}

#[derive(BeBytes)]
struct BebytesHeader {
    timestamp_ns: u64,
    sequence: u32,
    device_id: u32,
}

const BEBYTES_HEADER_SIZE: usize = 16;

fn readings_count(payload_size: usize) -> usize {
    payload_size.saturating_sub(BEBYTES_HEADER_SIZE) / 8
}

fn encode_payload(format: PayloadFormat, payload_size: usize, sequence: u32) -> Vec<u8> {
    let ts = nanos_as_u64();
    match format {
        PayloadFormat::Raw => {
            let size = payload_size.max(12);
            let mut buf = vec![0u8; size];
            buf[0..8].copy_from_slice(&ts.to_be_bytes());
            buf[8..12].copy_from_slice(&sequence.to_be_bytes());
            buf
        }
        PayloadFormat::Json => {
            let count = readings_count(payload_size);
            let readings = vec![0.0f64; count];
            let payload = JsonPayload {
                ts,
                seq: sequence,
                dev: 1,
                readings,
            };
            serde_json::to_vec(&payload).unwrap_or_default()
        }
        PayloadFormat::Bebytes => {
            let header = BebytesHeader {
                timestamp_ns: ts,
                sequence,
                device_id: 1,
            };
            let mut buf = header.to_be_bytes();
            let count = readings_count(payload_size);
            for _ in 0..count {
                buf.extend_from_slice(&0.0f64.to_be_bytes());
            }
            buf
        }
        PayloadFormat::CompressedJson => {
            let count = readings_count(payload_size);
            let readings = vec![0.0f64; count];
            let payload = JsonPayload {
                ts,
                seq: sequence,
                dev: 1,
                readings,
            };
            let json_bytes = serde_json::to_vec(&payload).unwrap_or_default();
            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(&json_bytes).ok();
            encoder.finish().unwrap_or_default()
        }
    }
}

fn decode_timestamp(format: PayloadFormat, payload: &[u8]) -> u64 {
    match format {
        PayloadFormat::Raw => {
            if payload.len() >= 8 {
                u64::from_be_bytes(payload[0..8].try_into().unwrap())
            } else {
                0
            }
        }
        PayloadFormat::Json => serde_json::from_slice::<JsonPayload>(payload)
            .map(|p| p.ts)
            .unwrap_or(0),
        PayloadFormat::Bebytes => {
            if payload.len() >= BEBYTES_HEADER_SIZE {
                BebytesHeader::try_from_be_bytes(payload)
                    .map(|(h, _)| h.timestamp_ns)
                    .unwrap_or(0)
            } else {
                0
            }
        }
        PayloadFormat::CompressedJson => {
            let mut decoder = GzDecoder::new(payload);
            let mut json_bytes = Vec::new();
            if decoder.read_to_end(&mut json_bytes).is_ok() {
                serde_json::from_slice::<JsonPayload>(&json_bytes)
                    .map(|p| p.ts)
                    .unwrap_or(0)
            } else {
                0
            }
        }
    }
}

fn decode_sequence(format: PayloadFormat, payload: &[u8]) -> u32 {
    match format {
        PayloadFormat::Raw => {
            if payload.len() >= 12 {
                u32::from_be_bytes(payload[8..12].try_into().unwrap())
            } else {
                0
            }
        }
        PayloadFormat::Json => serde_json::from_slice::<JsonPayload>(payload)
            .map(|p| p.seq)
            .unwrap_or(0),
        PayloadFormat::Bebytes => {
            if payload.len() >= BEBYTES_HEADER_SIZE {
                BebytesHeader::try_from_be_bytes(payload)
                    .map(|(h, _)| h.sequence)
                    .unwrap_or(0)
            } else {
                0
            }
        }
        PayloadFormat::CompressedJson => {
            let mut decoder = GzDecoder::new(payload);
            let mut json_bytes = Vec::new();
            if decoder.read_to_end(&mut json_bytes).is_ok() {
                serde_json::from_slice::<JsonPayload>(&json_bytes)
                    .map(|p| p.seq)
                    .unwrap_or(0)
            } else {
                0
            }
        }
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
    transport: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    quic_stream_strategy: Option<String>,
    quic_datagrams: bool,
    quic_flow_headers: bool,
    rate: u64,
    payload_format: String,
    actual_payload_bytes: usize,
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
    detrended_correlation: f64,
    raw_correlation: f64,
    inter_topic_spread_mean_us: f64,
    inter_topic_spread_p95_us: f64,
    inter_topic_spread_max_us: f64,
    window_size_ms: u64,
    window_count: usize,
    total_messages: u64,
    measured_rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    inter_arrival_cluster_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    spike_isolation_ratio: Option<f64>,
}

struct TraceRecord {
    topic_idx: u16,
    seq: u32,
    publish_ns: u64,
    receive_ns: u64,
    latency_us: u64,
    stream_id: u64,
}

struct StatsRecord {
    timestamp_ns: u64,
    rtt_us: u64,
    cwnd: u64,
    lost_packets: u64,
    congestion_events: u64,
    sent_packets: u64,
    stream_data_blocked: u64,
    data_blocked: u64,
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

#[allow(deprecated)]
fn strategy_display(s: mqtt5::transport::StreamStrategy) -> String {
    match s {
        mqtt5::transport::StreamStrategy::ControlOnly => "control-only".to_string(),
        mqtt5::transport::StreamStrategy::DataPerPublish => "per-publish".to_string(),
        mqtt5::transport::StreamStrategy::DataPerTopic
        | mqtt5::transport::StreamStrategy::DataPerSubscription => "per-topic".to_string(),
    }
}

fn bench_config(cmd: &BenchCommand, url: &str) -> BenchConfig {
    let filter = cmd.filter.clone().unwrap_or_else(|| cmd.topic.clone());
    let sample = encode_payload(cmd.payload_format, cmd.payload_size, 0);
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
        rate: cmd.rate,
        payload_format: format_name(cmd.payload_format),
        actual_payload_bytes: sample.len(),
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
    if cmd.quic_early_data {
        client.set_quic_early_data(true).await;
    }
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
    format: PayloadFormat,
    payload_size: usize,
    qos: QoS,
    running: &Arc<std::sync::atomic::AtomicBool>,
    published: &Arc<AtomicU64>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::with_capacity(pub_clients.len());
    for (i, pub_client) in pub_clients.into_iter().enumerate() {
        let topic = format!("{topic_base}/{i}");
        let running = Arc::clone(running);
        let published = Arc::clone(published);

        handles.push(tokio::spawn(async move {
            let mut seq = 0u32;
            while running.load(Ordering::Relaxed) {
                let payload = encode_payload(format, payload_size, seq);
                if publish_message(&pub_client, &topic, &payload, qos)
                    .await
                    .is_ok()
                {
                    published.fetch_add(1, Ordering::Relaxed);
                }
                seq = seq.wrapping_add(1);
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

    let format = cmd.payload_format;
    let mut sub_clients = Vec::with_capacity(cmd.subscribers);
    for i in 0..cmd.subscribers {
        let sub_client = connect_client(format!("{base_id}-sub-{i}"), &url, &cmd).await?;
        let received_clone = Arc::clone(&received);
        sub_client
            .subscribe(&filter, move |msg| {
                std::hint::black_box(decode_timestamp(format, &msg.payload));
                received_clone.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .context("failed to subscribe")?;
        sub_clients.push(sub_client);
    }

    eprintln!("subscribed {} client(s) to {filter}", cmd.subscribers);

    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let published = Arc::new(AtomicU64::new(0));

    eprintln!("warming up for {}s...", cmd.warmup);
    let handles = spawn_publishers(
        pub_clients,
        &topic,
        format,
        cmd.payload_size,
        cmd.qos,
        &running,
        &published,
    );

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
        config: bench_config(&cmd, &url),
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
    let format = cmd.payload_format;

    sub_client
        .subscribe(&filter, move |msg| {
            let sent_nanos = decode_timestamp(format, &msg.payload);
            if sent_nanos > 0 {
                let now_nanos = nanos_as_u64();
                let latency_us = (now_nanos.saturating_sub(sent_nanos)) / 1000;
                latencies_clone.lock().unwrap().push(latency_us);
            }
        })
        .await
        .context("failed to subscribe")?;

    let message_rate = 1000;
    let interval_us = 1_000_000 / message_rate;

    eprintln!("warming up for {}s...", cmd.warmup);
    send_timed_messages_formatted(
        &pub_client,
        &topic,
        format,
        cmd.payload_size,
        cmd.qos,
        cmd.warmup * message_rate,
        interval_us,
    )
    .await?;
    latencies.lock().unwrap().clear();

    eprintln!("measuring for {}s at {message_rate} msg/s...", cmd.duration);
    let measure_start = Instant::now();
    let measure_duration = Duration::from_secs(cmd.duration);
    let mut seq = 0u32;
    while measure_start.elapsed() < measure_duration {
        let payload = encode_payload(format, cmd.payload_size, seq);
        if publish_message(&pub_client, &topic, &payload, cmd.qos)
            .await
            .is_err()
        {
            eprintln!("connection lost after {seq} messages, reporting partial results");
            break;
        }
        seq = seq.wrapping_add(1);
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
        config: bench_config(&cmd, &url),
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

async fn send_timed_messages_formatted(
    client: &MqttClient,
    topic: &str,
    format: PayloadFormat,
    payload_size: usize,
    qos: QoS,
    count: u64,
    interval_us: u64,
) -> Result<()> {
    for seq in 0..count {
        #[allow(clippy::cast_possible_truncation)]
        let payload = encode_payload(format, payload_size, seq as u32);
        if publish_message(client, topic, &payload, qos).await.is_err() {
            eprintln!("connection lost during warmup after {seq} messages");
            break;
        }
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
        quic_early_data: cmd.quic_early_data,
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
            let mut cfg = bench_config(&cmd, &original_url);
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
    quic_early_data: bool,
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
        let quic_early_data = state.quic_early_data;
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
                if quic_early_data {
                    client.set_quic_early_data(true).await;
                }
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

async fn subscribe_hol_topics(
    sub_client: &MqttClient,
    num_topics: usize,
    format: PayloadFormat,
    topic_samples: &[Arc<std::sync::Mutex<Vec<TimestampedSample>>>],
    measure_start_nanos: &Arc<AtomicU64>,
    trace_records: Option<&Arc<std::sync::Mutex<Vec<TraceRecord>>>>,
) -> Result<()> {
    for (i, samples_vec) in topic_samples.iter().enumerate() {
        let topic_filter = format!("bench/hol/{i}");
        let samples_clone = Arc::clone(samples_vec);
        let start_nanos = Arc::clone(measure_start_nanos);
        let trace_clone = trace_records.map(Arc::clone);
        #[allow(clippy::cast_possible_truncation)]
        let topic_idx = i as u16;
        sub_client
            .subscribe(&topic_filter, move |msg| {
                let sent_nanos = decode_timestamp(format, &msg.payload);
                if sent_nanos > 0 {
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
                    if let Some(ref traces) = trace_clone {
                        let seq = decode_sequence(format, &msg.payload);
                        traces.lock().unwrap().push(TraceRecord {
                            topic_idx,
                            seq,
                            publish_ns: sent_nanos,
                            receive_ns: now_nanos,
                            latency_us,
                            stream_id: msg.stream_id.unwrap_or(0),
                        });
                    }
                }
            })
            .await
            .context("failed to subscribe")?;
    }
    eprintln!("subscribed to {num_topics} topics");
    Ok(())
}

fn spawn_quinn_stats_sampler(
    conn: Arc<quinn::Connection>,
    records: Arc<std::sync::Mutex<Vec<StatsRecord>>>,
    running: Arc<std::sync::atomic::AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(100));
        while running.load(Ordering::Relaxed) {
            interval.tick().await;
            let stats = conn.stats();
            let rtt = conn.rtt();
            #[allow(clippy::cast_possible_truncation)]
            let rtt_us = rtt.as_micros() as u64;
            records.lock().unwrap().push(StatsRecord {
                timestamp_ns: nanos_as_u64(),
                rtt_us,
                cwnd: stats.path.cwnd,
                lost_packets: stats.path.lost_packets,
                congestion_events: stats.path.congestion_events,
                sent_packets: stats.path.sent_packets,
                stream_data_blocked: stats.frame_rx.stream_data_blocked,
                data_blocked: stats.frame_rx.data_blocked,
            });
        }
    })
}

fn write_trace_csv(dir: &std::path::Path, records: &[TraceRecord]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("messages.csv");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&path)?);
    writeln!(
        out,
        "topic_idx,seq,publish_ns,receive_ns,latency_us,stream_id"
    )?;
    for r in records {
        writeln!(
            out,
            "{},{},{},{},{},{}",
            r.topic_idx, r.seq, r.publish_ns, r.receive_ns, r.latency_us, r.stream_id
        )?;
    }
    eprintln!(
        "wrote {} trace records to {}",
        records.len(),
        path.display()
    );
    Ok(())
}

fn write_stats_csv(dir: &std::path::Path, records: &[StatsRecord]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("quinn_stats.csv");
    let mut out = std::io::BufWriter::new(std::fs::File::create(&path)?);
    writeln!(out, "timestamp_ns,rtt_us,cwnd,lost_packets,congestion_events,sent_packets,stream_data_blocked,data_blocked")?;
    for r in records {
        writeln!(
            out,
            "{},{},{},{},{},{},{},{}",
            r.timestamp_ns,
            r.rtt_us,
            r.cwnd,
            r.lost_packets,
            r.congestion_events,
            r.sent_packets,
            r.stream_data_blocked,
            r.data_blocked
        )?;
    }
    eprintln!(
        "wrote {} stats records to {}",
        records.len(),
        path.display()
    );
    Ok(())
}

fn compute_inter_arrival_cluster_ratio(records: &[TraceRecord]) -> f64 {
    if records.len() < 2 {
        return 0.0;
    }
    let mut sorted: Vec<&TraceRecord> = records.iter().collect();
    sorted.sort_by_key(|r| r.receive_ns);

    let threshold_ns: u64 = 100_000;
    let mut clustered: u64 = 0;
    let mut cross_topic_pairs: u64 = 0;

    for i in 0..sorted.len() {
        for j in (i + 1)..sorted.len() {
            let delta = sorted[j].receive_ns.saturating_sub(sorted[i].receive_ns);
            if delta > threshold_ns {
                break;
            }
            if sorted[i].topic_idx != sorted[j].topic_idx {
                cross_topic_pairs += 1;
                clustered += 1;
            }
        }
    }

    if cross_topic_pairs == 0 {
        return 0.0;
    }

    let total_cross = total_cross_topic_adjacent_pairs(&sorted, threshold_ns);
    if total_cross == 0 {
        return 0.0;
    }
    as_f64_lossy(clustered) / as_f64_lossy(total_cross)
}

fn total_cross_topic_adjacent_pairs(sorted: &[&TraceRecord], window_ns: u64) -> u64 {
    let mut count: u64 = 0;
    for i in 0..sorted.len() {
        for j in (i + 1)..sorted.len() {
            let delta = sorted[j].receive_ns.saturating_sub(sorted[i].receive_ns);
            if delta > window_ns {
                break;
            }
            if sorted[i].topic_idx != sorted[j].topic_idx {
                count += 1;
            }
        }
    }
    count
}

struct SpikeEvent {
    topic: u16,
    receive_ns: u64,
}

fn compute_spike_isolation_ratio(records: &[TraceRecord], num_topics: u16) -> f64 {
    let mut per_topic: Vec<Vec<(u64, u64)>> = vec![Vec::new(); num_topics as usize];
    for r in records {
        if (r.topic_idx as usize) < per_topic.len() {
            per_topic[r.topic_idx as usize].push((r.receive_ns, r.latency_us));
        }
    }

    for topic_data in &mut per_topic {
        topic_data.sort_by_key(|(ns, _)| *ns);
    }

    let spike_window: usize = 50;
    let spike_threshold: f64 = 2.0;
    let co_occur_window_ns: u64 = 10_000_000;

    let mut all_spikes: Vec<SpikeEvent> = Vec::new();

    for (topic_idx, topic_data) in per_topic.iter().enumerate() {
        if topic_data.len() < spike_window {
            continue;
        }
        let latencies: Vec<f64> = topic_data.iter().map(|(_, l)| as_f64_lossy(*l)).collect();

        for i in spike_window..latencies.len() {
            let window = &latencies[i.saturating_sub(spike_window)..i];
            let mut window_sorted: Vec<f64> = window.to_vec();
            window_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let median = window_sorted[window_sorted.len() / 2];

            if median > 0.0 && latencies[i] > median * spike_threshold {
                #[allow(clippy::cast_possible_truncation)]
                all_spikes.push(SpikeEvent {
                    topic: topic_idx as u16,
                    receive_ns: topic_data[i].0,
                });
            }
        }
    }

    if all_spikes.is_empty() {
        return 0.0;
    }

    all_spikes.sort_by_key(|s| s.receive_ns);

    let mut co_occurring: u64 = 0;
    let total_spikes = all_spikes.len() as u64;

    for i in 0..all_spikes.len() {
        let spike = &all_spikes[i];
        let has_co_spike = all_spikes.iter().any(|other| {
            other.topic != spike.topic
                && other.receive_ns.abs_diff(spike.receive_ns) <= co_occur_window_ns
        });
        if has_co_spike {
            co_occurring += 1;
        }
    }

    as_f64_lossy(co_occurring) / as_f64_lossy(total_spikes)
}

fn finalize_hol_traces(
    results: &mut HolBlockingResults,
    num_topics: usize,
    trace_records: Option<&Arc<std::sync::Mutex<Vec<TraceRecord>>>>,
    stats_records: Option<&Arc<std::sync::Mutex<Vec<StatsRecord>>>>,
    trace_dir: Option<&std::path::PathBuf>,
) -> Result<()> {
    if let Some(traces) = trace_records {
        let records = traces.lock().unwrap();
        #[allow(clippy::cast_possible_truncation)]
        let ntopics = num_topics as u16;
        results.inter_arrival_cluster_ratio = Some(compute_inter_arrival_cluster_ratio(&records));
        results.spike_isolation_ratio = Some(compute_spike_isolation_ratio(&records, ntopics));
        eprintln!(
            "  inter_arrival_cluster_ratio: {:.4}, spike_isolation_ratio: {:.4}",
            results.inter_arrival_cluster_ratio.unwrap_or(0.0),
            results.spike_isolation_ratio.unwrap_or(0.0),
        );
    }

    if let Some(dir) = trace_dir {
        if let Some(traces) = trace_records {
            write_trace_csv(dir, &traces.lock().unwrap())?;
        }
        if let Some(stats) = stats_records {
            write_stats_csv(dir, &stats.lock().unwrap())?;
        }
    }
    Ok(())
}

async fn run_hol_blocking(cmd: BenchCommand) -> Result<()> {
    use std::sync::Mutex;

    let url = broker_url(&cmd);
    let pub_url = cmd.pub_url.clone().unwrap_or_else(|| url.clone());
    let base_id = base_client_id(&cmd, "hol");
    let num_topics = cmd.topics;
    let payload_size = cmd.payload_size.max(12);
    let trace_dir = cmd.trace_dir.clone();

    eprintln!("connecting pub to {pub_url}, sub to {url} for HOL blocking test with {num_topics} topics...");
    let pub_client = connect_client(format!("{base_id}-pub"), &pub_url, &cmd).await?;
    let sub_client = connect_client(format!("{base_id}-sub"), &url, &cmd).await?;

    let topic_samples: Vec<Arc<Mutex<Vec<TimestampedSample>>>> = (0..num_topics)
        .map(|_| Arc::new(Mutex::new(Vec::with_capacity(100_000))))
        .collect();
    let trace_records: Option<Arc<Mutex<Vec<TraceRecord>>>> = trace_dir
        .as_ref()
        .map(|_| Arc::new(Mutex::new(Vec::with_capacity(200_000))));

    let format = cmd.payload_format;
    let measure_start_nanos = Arc::new(AtomicU64::new(0));
    subscribe_hol_topics(
        &sub_client,
        num_topics,
        format,
        &topic_samples,
        &measure_start_nanos,
        trace_records.as_ref(),
    )
    .await?;

    let per_topic_interval_us = if cmd.rate > 0 {
        #[allow(clippy::cast_possible_truncation)]
        let interval = 1_000_000u64 * (num_topics as u64) / cmd.rate;
        Some(interval)
    } else {
        None
    };
    let rate_label = if cmd.rate > 0 {
        format!("{} msg/s", cmd.rate)
    } else {
        "unlimited".to_string()
    };

    let running = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let published = Arc::new(AtomicU64::new(0));
    let pub_cfg = HolPublishConfig {
        num_topics,
        format,
        payload_size,
        per_topic_interval_us,
        qos: cmd.qos,
    };

    eprintln!("warming up for {}s at {rate_label}...", cmd.warmup);
    run_hol_warmup(
        &pub_client,
        &pub_cfg,
        &running,
        &published,
        &topic_samples,
        trace_records.as_ref(),
        cmd.warmup,
    )
    .await;

    let stats_records: Option<Arc<Mutex<Vec<StatsRecord>>>> = trace_dir.as_ref().map(|_| {
        #[allow(clippy::cast_possible_truncation)]
        let cap = (cmd.duration * 10) as usize + 10;
        Arc::new(Mutex::new(Vec::with_capacity(cap)))
    });

    let (elapsed, total_published) = run_hol_measure_phase(
        &sub_client,
        &pub_client,
        &pub_cfg,
        &running,
        &published,
        &measure_start_nanos,
        stats_records.as_ref(),
        &rate_label,
        cmd.duration,
    )
    .await;

    let results = finalize_and_report_hol(
        &topic_samples,
        elapsed,
        total_published,
        num_topics,
        trace_records.as_ref(),
        stats_records.as_ref(),
        trace_dir.as_ref(),
    )?;

    let output = BenchOutput {
        mode: "hol-blocking".to_string(),
        config: bench_config(&cmd, &url),
        results: BenchResults::HolBlocking(results),
    };
    println!("{}", serde_json::to_string_pretty(&output)?);

    pub_client.disconnect().await.ok();
    sub_client.disconnect().await.ok();
    Ok(())
}

fn finalize_and_report_hol(
    topic_samples: &[Arc<std::sync::Mutex<Vec<TimestampedSample>>>],
    elapsed: f64,
    total_published: u64,
    num_topics: usize,
    trace_records: Option<&Arc<std::sync::Mutex<Vec<TraceRecord>>>>,
    stats_records: Option<&Arc<std::sync::Mutex<Vec<StatsRecord>>>>,
    trace_dir: Option<&PathBuf>,
) -> Result<HolBlockingResults> {
    let mut results = gather_hol_results(topic_samples, elapsed);
    finalize_hol_traces(
        &mut results,
        num_topics,
        trace_records,
        stats_records,
        trace_dir,
    )?;

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
    eprintln!(
        "  detrended_correlation: {:.4}, inter_topic_spread: mean={:.1}us p95={:.1}us max={:.1}us",
        results.detrended_correlation,
        results.inter_topic_spread_mean_us,
        results.inter_topic_spread_p95_us,
        results.inter_topic_spread_max_us,
    );

    Ok(results)
}

#[allow(clippy::too_many_arguments)]
async fn run_hol_measure_phase(
    sub_client: &MqttClient,
    pub_client: &MqttClient,
    pub_cfg: &HolPublishConfig,
    running: &Arc<std::sync::atomic::AtomicBool>,
    published: &Arc<AtomicU64>,
    measure_start_nanos: &Arc<AtomicU64>,
    stats_records: Option<&Arc<std::sync::Mutex<Vec<StatsRecord>>>>,
    rate_label: &str,
    duration: u64,
) -> (f64, u64) {
    running.store(true, Ordering::SeqCst);
    let stats_handle = if let Some(stats) = stats_records {
        sub_client.quic_connection().await.map(|conn| {
            eprintln!("  quinn stats sampler active (100ms interval)");
            spawn_quinn_stats_sampler(conn, Arc::clone(stats), Arc::clone(running))
        })
    } else {
        None
    };

    eprintln!("measuring for {duration}s at {rate_label}...");
    measure_start_nanos.store(nanos_as_u64(), Ordering::SeqCst);
    let measure_wall = Instant::now();
    let measure_handles = spawn_hol_publishers(pub_client, pub_cfg, running, published);
    tokio::time::sleep(Duration::from_secs(duration)).await;
    running.store(false, Ordering::SeqCst);
    for handle in measure_handles {
        handle.await.ok();
    }
    if let Some(handle) = stats_handle {
        handle.await.ok();
    }

    let elapsed = measure_wall.elapsed().as_secs_f64();
    let total_published = published.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(500)).await;
    (elapsed, total_published)
}

async fn run_hol_warmup(
    pub_client: &MqttClient,
    pub_cfg: &HolPublishConfig,
    running: &Arc<std::sync::atomic::AtomicBool>,
    published: &Arc<AtomicU64>,
    topic_samples: &[Arc<std::sync::Mutex<Vec<TimestampedSample>>>],
    trace_records: Option<&Arc<std::sync::Mutex<Vec<TraceRecord>>>>,
    warmup_secs: u64,
) {
    let warmup_handles = spawn_hol_publishers(pub_client, pub_cfg, running, published);
    tokio::time::sleep(Duration::from_secs(warmup_secs)).await;
    running.store(false, Ordering::SeqCst);
    for handle in warmup_handles {
        handle.await.ok();
    }
    for sv in topic_samples {
        sv.lock().unwrap().clear();
    }
    if let Some(traces) = trace_records {
        traces.lock().unwrap().clear();
    }
    published.store(0, Ordering::SeqCst);
}

struct HolPublishConfig {
    num_topics: usize,
    format: PayloadFormat,
    payload_size: usize,
    per_topic_interval_us: Option<u64>,
    qos: QoS,
}

fn spawn_hol_publishers(
    pub_client: &MqttClient,
    cfg: &HolPublishConfig,
    running: &Arc<std::sync::atomic::AtomicBool>,
    published: &Arc<AtomicU64>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let mut handles = Vec::with_capacity(cfg.num_topics);
    for topic_idx in 0..cfg.num_topics {
        let client = pub_client.clone();
        let running = Arc::clone(running);
        let published = Arc::clone(published);
        let format = cfg.format;
        let payload_size = cfg.payload_size;
        let per_topic_interval_us = cfg.per_topic_interval_us;
        let qos = cfg.qos;

        handles.push(tokio::spawn(async move {
            let topic = format!("bench/hol/{topic_idx}");
            let mut seq = 0u32;
            while running.load(Ordering::Relaxed) {
                let payload = encode_payload(format, payload_size, seq);
                if publish_message(&client, &topic, &payload, qos)
                    .await
                    .is_ok()
                {
                    published.fetch_add(1, Ordering::Relaxed);
                }
                seq = seq.wrapping_add(1);
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
    let detrended_corr = detrended_correlation(&all_samples, window_size_ms);
    let (spread_mean, spread_p95, spread_max) = inter_topic_spread(&all_samples, 100);
    let measured_rate = as_f64_lossy(total_messages) / elapsed_secs;

    HolBlockingResults {
        topics: topic_results,
        windowed_correlation: windowed_corr,
        detrended_correlation: detrended_corr,
        raw_correlation,
        inter_topic_spread_mean_us: spread_mean,
        inter_topic_spread_p95_us: spread_p95,
        inter_topic_spread_max_us: spread_max,
        window_size_ms,
        window_count,
        total_messages,
        measured_rate,
        inter_arrival_cluster_ratio: None,
        spike_isolation_ratio: None,
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

fn compute_windowed_means(
    topic_samples: &[Vec<TimestampedSample>],
    window_ms: u64,
) -> (Vec<Vec<f64>>, usize) {
    let max_time_us = topic_samples
        .iter()
        .flat_map(|s| s.iter().map(|ts| ts.received_at_us))
        .max()
        .unwrap_or(0);

    let bucket_us = window_ms * 1000;
    if bucket_us == 0 || max_time_us == 0 {
        return (vec![Vec::new(); topic_samples.len()], 0);
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

    (per_topic_means, valid_windows)
}

fn mean_pairwise_correlation(per_topic_series: &[Vec<f64>]) -> f64 {
    let mut total_r = 0.0;
    let mut pair_count = 0u64;
    for i in 0..per_topic_series.len() {
        for j in (i + 1)..per_topic_series.len() {
            let r = pearson_pair_f64(&per_topic_series[i], &per_topic_series[j]);
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

fn windowed_correlation(topic_samples: &[Vec<TimestampedSample>], window_ms: u64) -> (f64, usize) {
    if topic_samples.len() < 2 {
        return (0.0, 0);
    }

    let (per_topic_means, valid_windows) = compute_windowed_means(topic_samples, window_ms);
    if valid_windows < 2 {
        return (0.0, valid_windows);
    }

    (mean_pairwise_correlation(&per_topic_means), valid_windows)
}

fn detrended_correlation(topic_samples: &[Vec<TimestampedSample>], window_ms: u64) -> f64 {
    if topic_samples.len() < 2 {
        return 0.0;
    }

    let (per_topic_means, valid_windows) = compute_windowed_means(topic_samples, window_ms);
    if valid_windows < 3 {
        return 0.0;
    }

    let diffs: Vec<Vec<f64>> = per_topic_means
        .iter()
        .map(|means| means.windows(2).map(|w| w[1] - w[0]).collect())
        .collect();

    mean_pairwise_correlation(&diffs)
}

fn inter_topic_spread(topic_samples: &[Vec<TimestampedSample>], window_ms: u64) -> (f64, f64, f64) {
    if topic_samples.len() < 2 {
        return (0.0, 0.0, 0.0);
    }

    let (per_topic_means, valid_windows) = compute_windowed_means(topic_samples, window_ms);
    if valid_windows == 0 {
        return (0.0, 0.0, 0.0);
    }

    let num_topics = per_topic_means.len();
    let mut spreads = Vec::with_capacity(valid_windows);

    for w in 0..valid_windows {
        let mut min_mean = f64::MAX;
        let mut max_mean = f64::MIN;
        for topic_means in &per_topic_means[..num_topics] {
            let mean = topic_means[w];
            if mean < min_mean {
                min_mean = mean;
            }
            if mean > max_mean {
                max_mean = mean;
            }
        }
        spreads.push(max_mean - min_mean);
    }

    spreads.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let sum: f64 = spreads.iter().sum();
    let mean = sum / usize_as_f64_lossy(spreads.len());
    let p95_idx = (spreads.len() * 95) / 100;
    let p95 = spreads[p95_idx.min(spreads.len() - 1)];
    let max = spreads.last().copied().unwrap_or(0.0);

    (mean, p95, max)
}
