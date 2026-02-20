#![allow(clippy::large_futures)]
#![allow(clippy::struct_excessive_bools)]

use anyhow::{Context, Result};
use clap::Args;
use dialoguer::{Input, Select};
use mqtt5::client::auth_handlers::{JwtAuthHandler, ScramSha256AuthHandler};
use mqtt5::time::Duration;
#[cfg(feature = "codec")]
use mqtt5::{CodecRegistry, DeflateCodec, GzipCodec};
use mqtt5::{
    ConnectOptions, ConnectionEvent, Message, MqttClient, ProtocolVersion, PublishOptions, QoS,
    WillMessage,
};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::signal;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use super::parsers::{
    calculate_wait_until, duration_secs_to_u32, parse_duration_millis, parse_duration_secs,
};

#[derive(Args)]
pub struct PubCommand {
    /// MQTT topic to publish to
    #[arg(long, short)]
    pub topic: Option<String>,

    /// Message to publish
    #[arg(long, short)]
    pub message: Option<String>,

    /// Read message from file
    #[arg(long, short)]
    pub file: Option<String>,

    /// Read message from stdin
    #[arg(long)]
    pub stdin: bool,

    /// Full broker URL for TLS/WebSocket/QUIC (e.g., <mqtts://host:8883>, <wss://host/mqtt>)
    #[arg(long, short = 'U', conflicts_with_all = &["host", "port"])]
    pub url: Option<String>,

    /// Broker hostname (builds mqtt:// URL, use --url for TLS/WebSocket/QUIC)
    #[arg(long, short = 'H', default_value = "localhost")]
    pub host: String,

    /// Broker port (used with --host)
    #[arg(long, short, default_value = "1883")]
    pub port: u16,

    /// Quality of Service level (0, 1, or 2)
    #[arg(long, short, value_parser = parse_qos)]
    pub qos: Option<QoS>,

    /// Retain message
    #[arg(long, short)]
    pub retain: bool,

    /// Message expiry interval in seconds (0 = no expiry)
    #[arg(long)]
    pub message_expiry_interval: Option<u32>,

    /// Topic alias (1-65535) for repeated publishing to same topic
    #[arg(long)]
    pub topic_alias: Option<u16>,

    /// Response topic for request/response pattern (MQTT 5.0)
    #[arg(long)]
    pub response_topic: Option<String>,

    /// Correlation data for request/response pattern (MQTT 5.0, hex-encoded)
    #[arg(long)]
    pub correlation_data: Option<String>,

    /// Wait for response after publishing (requires --response-topic)
    #[arg(long)]
    pub wait_response: bool,

    /// Timeout when waiting for response (e.g., 30s, 1m) (default: 30s)
    #[arg(long, default_value = "30", value_parser = parse_duration_secs)]
    pub timeout: u64,

    /// Number of responses to wait for (default: 1, 0 = unlimited until timeout)
    #[arg(long, default_value = "1")]
    pub response_count: u32,

    /// Output format for responses: raw, json, verbose
    #[arg(long, default_value = "raw", value_parser = ["raw", "json", "verbose"])]
    pub output_format: String,

    /// Username for authentication
    #[arg(long, short)]
    pub username: Option<String>,

    /// Password for authentication
    #[arg(long, short = 'P')]
    pub password: Option<String>,

    /// Authentication method: password, scram, jwt (default: password)
    #[arg(long, value_parser = ["password", "scram", "jwt"])]
    pub auth_method: Option<String>,

    /// JWT token for JWT authentication
    #[arg(long)]
    pub jwt_token: Option<String>,

    /// Client ID
    #[arg(long, short)]
    pub client_id: Option<String>,

    /// Skip prompts and use defaults/fail if required args missing
    #[arg(long)]
    pub non_interactive: bool,

    /// Don't clean start (resume existing session)
    #[arg(long = "no-clean-start")]
    pub no_clean_start: bool,

    /// Session expiry interval (e.g., 1h, 30m) (0 = expire on disconnect)
    #[arg(long, value_parser = parse_duration_secs)]
    pub session_expiry: Option<u64>,

    /// Keep alive interval (e.g., 60s, 1m) (default: 60s)
    #[arg(long, short = 'k', default_value = "60", value_parser = parse_duration_secs)]
    pub keep_alive: u64,

    /// MQTT protocol version (3.1.1 or 5, default: 5)
    #[arg(long, value_parser = parse_protocol_version)]
    pub protocol_version: Option<ProtocolVersion>,

    /// Will topic (last will and testament)
    #[arg(long)]
    pub will_topic: Option<String>,

    /// Will message payload
    #[arg(long)]
    pub will_message: Option<String>,

    /// Will `QoS` level (0, 1, or 2)
    #[arg(long, value_parser = parse_qos)]
    pub will_qos: Option<QoS>,

    /// Will retain flag
    #[arg(long)]
    pub will_retain: bool,

    /// TLS certificate file (PEM format) for secure connections
    #[arg(long)]
    pub cert: Option<PathBuf>,

    /// TLS private key file (PEM format) for secure connections
    #[arg(long)]
    pub key: Option<PathBuf>,

    /// TLS CA certificate file (PEM format) for server verification
    #[arg(long)]
    pub ca_cert: Option<PathBuf>,

    /// Skip certificate verification for TLS/QUIC connections (insecure, for testing only)
    #[arg(long)]
    pub insecure: bool,

    /// Will delay interval (e.g., 5m, 1h)
    #[arg(long, value_parser = parse_duration_secs)]
    pub will_delay: Option<u64>,

    /// Keep connection alive after publishing (for testing will messages)
    #[arg(long, hide = true)]
    pub keep_alive_after_publish: bool,

    /// Enable automatic reconnection when broker disconnects
    #[arg(long)]
    pub auto_reconnect: bool,

    /// QUIC stream strategy (control-only, per-publish, per-topic, per-subscription)
    #[arg(long, value_parser = parse_stream_strategy)]
    pub quic_stream_strategy: Option<mqtt5::transport::StreamStrategy>,

    /// Enable `MQoQ` flow headers for stream state tracking
    #[arg(long)]
    pub quic_flow_headers: bool,

    /// Flow expiration interval (e.g., 5m, 1h) (default: 5m)
    #[arg(long, default_value = "300", value_parser = parse_duration_secs)]
    pub quic_flow_expire: u64,

    /// Maximum concurrent QUIC streams
    #[arg(long)]
    pub quic_max_streams: Option<usize>,

    /// Enable QUIC datagrams for unreliable transport
    #[arg(long)]
    pub quic_datagrams: bool,

    /// QUIC connection timeout (e.g., 30s, 1m) (default: 30s)
    #[arg(long, default_value = "30", value_parser = parse_duration_secs)]
    pub quic_connect_timeout: u64,

    /// Delay before publishing (e.g., 5s, 1m30s)
    #[arg(long, value_parser = parse_duration_secs)]
    pub delay: Option<u64>,

    /// Repeat publishing N times (0 = infinite until Ctrl+C)
    #[arg(long)]
    pub repeat: Option<u64>,

    /// Interval between repeated publishes in ms (e.g., 1000, 1s, 500ms)
    #[arg(long, value_parser = parse_duration_millis, requires = "repeat")]
    pub interval: Option<u64>,

    /// Schedule publish at specific time (e.g., 14:30, 14:30:00, 2025-01-15T14:30:00)
    #[arg(long, conflicts_with = "delay")]
    pub at: Option<String>,

    /// OpenTelemetry OTLP endpoint (e.g., `http://localhost:4317`)
    #[cfg(feature = "opentelemetry")]
    #[arg(long)]
    pub otel_endpoint: Option<String>,

    /// OpenTelemetry service name (default: mqttv5-pub)
    #[cfg(feature = "opentelemetry")]
    #[arg(long, default_value = "mqttv5-pub")]
    pub otel_service_name: String,

    /// OpenTelemetry sampling ratio (0.0-1.0, default: 1.0)
    #[cfg(feature = "opentelemetry")]
    #[arg(long, default_value = "1.0")]
    pub otel_sampling: f64,

    /// Compress payload using codec (gzip, deflate)
    #[cfg(feature = "codec")]
    #[arg(long, value_parser = ["gzip", "deflate"])]
    pub codec: Option<String>,

    /// Codec compression level (0-9, default: 6)
    #[cfg(feature = "codec")]
    #[arg(long, default_value = "6", requires = "codec")]
    pub codec_level: u32,

    /// Minimum payload size for compression in bytes (default: 128)
    #[cfg(feature = "codec")]
    #[arg(long, default_value = "128", requires = "codec")]
    pub codec_min_size: usize,
}

fn parse_qos(s: &str) -> Result<QoS, String> {
    match s {
        "0" => Ok(QoS::AtMostOnce),
        "1" => Ok(QoS::AtLeastOnce),
        "2" => Ok(QoS::ExactlyOnce),
        _ => Err(format!("QoS must be 0, 1, or 2, got: {s}")),
    }
}

fn parse_protocol_version(s: &str) -> Result<ProtocolVersion, String> {
    match s {
        "3.1.1" | "311" | "4" => Ok(ProtocolVersion::V311),
        "5" | "5.0" => Ok(ProtocolVersion::V5),
        _ => Err(format!("Invalid protocol version: {s}. Use '3.1.1' or '5'")),
    }
}

fn parse_stream_strategy(s: &str) -> Result<mqtt5::transport::StreamStrategy, String> {
    use mqtt5::transport::StreamStrategy;
    match s.to_lowercase().as_str() {
        "control-only" | "control" => Ok(StreamStrategy::ControlOnly),
        "per-publish" | "publish" => Ok(StreamStrategy::DataPerPublish),
        "per-topic" | "topic" => Ok(StreamStrategy::DataPerTopic),
        "per-subscription" | "subscription" => Ok(StreamStrategy::DataPerSubscription),
        _ => Err(format!(
            "Invalid stream strategy: {s}. Valid: control-only, per-publish, per-topic, per-subscription"
        )),
    }
}

fn prompt_topic_and_qos(cmd: &mut PubCommand) -> Result<(String, QoS)> {
    if cmd.topic.is_none() && !cmd.non_interactive {
        let topic = Input::<String>::new()
            .with_prompt("MQTT topic (e.g., sensors/temperature, home/status)")
            .interact()
            .context("Failed to get topic input")?;
        cmd.topic = Some(topic);
    }

    let topic = cmd.topic.clone().ok_or_else(|| {
        anyhow::anyhow!("Topic is required. Use --topic or run without --non-interactive")
    })?;

    if topic.is_empty() {
        anyhow::bail!("Topic cannot be empty");
    }
    if topic.contains("//") {
        anyhow::bail!(
            "Invalid topic '{}' - cannot have empty segments\nDid you mean '{}'?",
            topic,
            topic.replace("//", "/")
        );
    }
    if topic.ends_with('/') {
        anyhow::bail!(
            "Invalid topic '{}' - cannot end with '/'\nDid you mean '{}'?",
            topic,
            topic.trim_end_matches('/')
        );
    }

    if cmd.wait_response && cmd.response_topic.is_none() {
        anyhow::bail!("--response-topic is required when using --wait-response");
    }

    let qos = if cmd.qos.is_none() && !cmd.non_interactive {
        let qos_options = vec![
            "0 (At most once - fire and forget)",
            "1 (At least once - acknowledged)",
            "2 (Exactly once - assured)",
        ];
        let selection = Select::new()
            .with_prompt("Quality of Service level")
            .items(&qos_options)
            .default(0)
            .interact()
            .context("Failed to get QoS selection")?;

        match selection {
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => QoS::AtMostOnce,
        }
    } else {
        cmd.qos.unwrap_or(QoS::AtMostOnce)
    };

    if cmd.wait_response && qos == QoS::AtMostOnce {
        warn!("Using --wait-response with QoS 0 may be unreliable; consider using -q 1 or -q 2");
    }

    Ok((topic, qos))
}

fn build_connect_options(cmd: &PubCommand, client_id: &str) -> ConnectOptions {
    let mut options = ConnectOptions::new(client_id.to_owned())
        .with_clean_start(!cmd.no_clean_start)
        .with_keep_alive(Duration::from_secs(cmd.keep_alive));

    if cmd.auto_reconnect {
        options = options.with_automatic_reconnect(true);
    }

    if let Some(version) = cmd.protocol_version {
        options = options.with_protocol_version(version);
    }

    if let Some(expiry) = cmd.session_expiry {
        options = options.with_session_expiry_interval(duration_secs_to_u32(expiry));
    }

    options
}

async fn configure_auth(
    client: &MqttClient,
    options: &mut ConnectOptions,
    cmd: &PubCommand,
) -> Result<()> {
    match cmd.auth_method.as_deref() {
        Some("scram") => {
            let username = cmd.username.clone().ok_or_else(|| {
                anyhow::anyhow!("--username is required for SCRAM authentication")
            })?;
            let password = cmd.password.clone().ok_or_else(|| {
                anyhow::anyhow!("--password is required for SCRAM authentication")
            })?;
            *options = std::mem::take(options)
                .with_credentials(username.clone(), Vec::new())
                .with_authentication_method("SCRAM-SHA-256");

            let handler = ScramSha256AuthHandler::new(username, password);
            client.set_auth_handler(handler).await;
            debug!("SCRAM-SHA-256 authentication configured");
        }
        Some("jwt") => {
            let token = cmd
                .jwt_token
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--jwt-token is required for JWT authentication"))?;
            *options = std::mem::take(options).with_authentication_method("JWT");

            let handler = JwtAuthHandler::new(token);
            client.set_auth_handler(handler).await;
            debug!("JWT authentication configured");
        }
        Some("password") | None => {
            if let (Some(username), Some(password)) = (cmd.username.clone(), cmd.password.clone()) {
                *options =
                    std::mem::take(options).with_credentials(username, password.into_bytes());
            } else if let Some(username) = cmd.username.clone() {
                *options = std::mem::take(options).with_credentials(username, Vec::new());
            }
        }
        Some(other) => anyhow::bail!("Unknown auth method: {other}"),
    }
    Ok(())
}

fn configure_will(options: &mut ConnectOptions, cmd: &PubCommand) {
    if let Some(topic) = cmd.will_topic.clone() {
        let payload = cmd.will_message.clone().unwrap_or_default();
        let mut will = WillMessage::new(topic, payload.into_bytes()).with_retain(cmd.will_retain);

        if let Some(qos) = cmd.will_qos {
            will = will.with_qos(qos);
        }

        if let Some(delay) = cmd.will_delay {
            will.properties.will_delay_interval = Some(duration_secs_to_u32(delay));
        }

        *options = std::mem::take(options).with_will(will);
    }
}

async fn configure_quic_transport(client: &MqttClient, cmd: &PubCommand) {
    if let Some(strategy) = cmd.quic_stream_strategy {
        client.set_quic_stream_strategy(strategy).await;
        debug!("QUIC stream strategy: {:?}", strategy);
    }
    if cmd.quic_flow_headers {
        client.set_quic_flow_headers(true).await;
        debug!("QUIC flow headers enabled");
    }
    client
        .set_quic_flow_expire(std::time::Duration::from_secs(cmd.quic_flow_expire))
        .await;
    if let Some(max) = cmd.quic_max_streams {
        client.set_quic_max_streams(Some(max)).await;
        debug!("QUIC max streams: {max}");
    }
    if cmd.quic_datagrams {
        client.set_quic_datagrams(true).await;
        debug!("QUIC datagrams enabled");
    }
    client
        .set_quic_connect_timeout(Duration::from_secs(cmd.quic_connect_timeout))
        .await;
}

async fn configure_tls_certs(
    client: &MqttClient,
    broker_url: &str,
    cmd: &PubCommand,
) -> Result<()> {
    let is_secure = broker_url.starts_with("ssl://")
        || broker_url.starts_with("mqtts://")
        || broker_url.starts_with("quics://");
    let has_certs = cmd.cert.is_some() || cmd.key.is_some() || cmd.ca_cert.is_some();

    if is_secure && has_certs {
        let cert_pem = if let Some(cert_path) = &cmd.cert {
            Some(std::fs::read(cert_path).with_context(|| {
                format!("Failed to read certificate file: {}", cert_path.display())
            })?)
        } else {
            None
        };
        let key_pem = if let Some(key_path) = &cmd.key {
            Some(
                std::fs::read(key_path)
                    .with_context(|| format!("Failed to read key file: {}", key_path.display()))?,
            )
        } else {
            None
        };
        let ca_pem = if let Some(ca_path) = &cmd.ca_cert {
            Some(std::fs::read(ca_path).with_context(|| {
                format!("Failed to read CA certificate file: {}", ca_path.display())
            })?)
        } else {
            None
        };

        client.set_tls_config(cert_pem, key_pem, ca_pem).await;
    }
    Ok(())
}

async fn setup_response_subscription(
    client: &MqttClient,
    cmd: &PubCommand,
    correlation_data: Option<Vec<u8>>,
) -> Result<(Arc<AtomicU32>, Arc<Notify>)> {
    let received_count = Arc::new(AtomicU32::new(0));
    let done_notify = Arc::new(Notify::new());

    if cmd.wait_response {
        let response_topic = cmd.response_topic.as_ref().unwrap().clone();
        let expected_correlation = correlation_data;
        let target_count = cmd.response_count;
        let output_format = cmd.output_format.clone();
        let received_clone = received_count.clone();
        let done_clone = done_notify.clone();

        client
            .subscribe(&response_topic, move |msg: Message| {
                if let Some(ref expected) = expected_correlation {
                    match &msg.properties.correlation_data {
                        Some(received) if received == expected => {}
                        _ => return,
                    }
                }

                format_response_message(&output_format, &msg);

                let count = received_clone.fetch_add(1, Ordering::Relaxed) + 1;
                if target_count > 0 && count >= target_count {
                    done_clone.notify_one();
                }
            })
            .await?;

        debug!(
            "SUBACK received for '{}', subscription ready before publish",
            response_topic
        );
    }

    Ok((received_count, done_notify))
}

fn format_response_message(output_format: &str, msg: &Message) {
    match output_format {
        "json" => {
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&msg.payload) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json).unwrap_or_default()
                );
            } else {
                println!("{}", String::from_utf8_lossy(&msg.payload));
            }
        }
        "verbose" => {
            println!("Topic: {}", msg.topic);
            if let Some(corr) = &msg.properties.correlation_data {
                println!("Correlation: {}", hex::encode(corr));
            }
            println!("Payload: {}", String::from_utf8_lossy(&msg.payload));
            println!("---");
        }
        _ => {
            println!("{}", String::from_utf8_lossy(&msg.payload));
        }
    }
}

async fn publish_loop(
    client: &MqttClient,
    topic: &str,
    message: &str,
    cmd: &PubCommand,
    qos: QoS,
    correlation_data: Option<&Vec<u8>>,
) -> Result<()> {
    let has_properties = cmd.retain
        || cmd.message_expiry_interval.is_some()
        || cmd.topic_alias.is_some()
        || cmd.response_topic.is_some()
        || correlation_data.is_some();

    let repeat_count = cmd.repeat.unwrap_or(1);
    let interval_millis = cmd.interval.unwrap_or(1000);
    let infinite_repeat = cmd.repeat == Some(0);
    let mut iteration = 0u64;

    loop {
        iteration += 1;

        if has_properties {
            publish_with_properties(client, topic, message, cmd, qos, correlation_data).await?;
        } else {
            publish_simple(client, topic, message, qos).await?;
        }

        print_publish_result(cmd, iteration, topic, qos);

        if !infinite_repeat && iteration >= repeat_count {
            break;
        }

        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_millis(interval_millis)) => {}
            _ = signal::ctrl_c() => {
                println!("\n✓ Interrupted after {iteration} publishes");
                client.disconnect().await?;
                return Ok(());
            }
        }
    }

    Ok(())
}

async fn publish_with_properties(
    client: &MqttClient,
    topic: &str,
    message: &str,
    cmd: &PubCommand,
    qos: QoS,
    correlation_data: Option<&Vec<u8>>,
) -> Result<()> {
    let mut options = PublishOptions {
        qos,
        retain: cmd.retain,
        ..Default::default()
    };
    options.properties.message_expiry_interval = cmd.message_expiry_interval;
    options.properties.topic_alias = cmd.topic_alias;
    options
        .properties
        .response_topic
        .clone_from(&cmd.response_topic);
    options.properties.correlation_data = correlation_data.cloned();
    client
        .publish_with_options(topic, message.as_bytes(), options)
        .await?;
    Ok(())
}

async fn publish_simple(client: &MqttClient, topic: &str, message: &str, qos: QoS) -> Result<()> {
    match qos {
        QoS::AtMostOnce => {
            client.publish(topic, message.as_bytes()).await?;
        }
        QoS::AtLeastOnce => {
            client.publish_qos1(topic, message.as_bytes()).await?;
        }
        QoS::ExactlyOnce => {
            client.publish_qos2(topic, message.as_bytes()).await?;
        }
    }
    Ok(())
}

fn print_publish_result(cmd: &PubCommand, iteration: u64, topic: &str, qos: QoS) {
    if cmd.repeat.is_some() {
        println!(
            "✓ Published message {} to '{}' (QoS {})",
            iteration, topic, qos as u8
        );
    } else {
        println!("✓ Published message to '{}' (QoS {})", topic, qos as u8);
    }
    if cmd.retain {
        println!("  Message retained on broker");
    }
}

async fn wait_for_response(
    cmd: &PubCommand,
    done_notify: Arc<Notify>,
    received_count: Arc<AtomicU32>,
    client: &MqttClient,
) -> Result<()> {
    if !cmd.wait_response {
        return Ok(());
    }

    let timeout_secs = cmd.timeout;
    let target_count = cmd.response_count;

    println!(
        "Waiting for response on '{}'...",
        cmd.response_topic.as_ref().unwrap()
    );

    tokio::select! {
        () = done_notify.notified() => {
        }
        () = tokio::time::sleep(std::time::Duration::from_secs(timeout_secs)) => {
            let count = received_count.load(Ordering::Relaxed);
            if count == 0 {
                client.disconnect().await?;
                anyhow::bail!("Timeout: no response received within {timeout_secs}s");
            } else if target_count > 0 && count < target_count {
                eprintln!(
                    "Warning: timeout after receiving {count} of {target_count} expected responses"
                );
            }
        }
        _ = signal::ctrl_c() => {
            println!("\n✓ Interrupted");
        }
    }

    Ok(())
}

async fn keep_alive_loop(client: &MqttClient, auto_reconnect: bool) -> Result<()> {
    info!("Keeping connection alive (--keep-alive-after-publish)");

    if auto_reconnect {
        client
            .on_connection_event(move |event| match event {
                ConnectionEvent::Connecting => {
                    info!("Connecting to broker...");
                }
                ConnectionEvent::Connected { session_present } => {
                    if session_present {
                        info!("✓ Reconnected (session present)");
                    } else {
                        info!("✓ Reconnected (new session)");
                    }
                }
                ConnectionEvent::Disconnected { .. } => {
                    warn!("⚠ Disconnected from broker, attempting reconnection...");
                }
                ConnectionEvent::Reconnecting { attempt } => {
                    info!("Reconnecting (attempt {})...", attempt);
                }
                ConnectionEvent::ReconnectFailed { error } => {
                    warn!("⚠ Reconnection failed: {error}");
                }
            })
            .await?;
    }

    match signal::ctrl_c().await {
        Ok(()) => {
            println!("\n✓ Received Ctrl+C, disconnecting...");
        }
        Err(err) => {
            anyhow::bail!("Unable to listen for shutdown signal: {err}");
        }
    }

    Ok(())
}

pub async fn execute(mut cmd: PubCommand, verbose: bool, debug: bool) -> Result<()> {
    #[cfg(feature = "opentelemetry")]
    let has_otel = cmd.otel_endpoint.is_some();

    #[cfg(not(feature = "opentelemetry"))]
    let has_otel = false;

    if !has_otel {
        crate::init_basic_tracing(verbose, debug);
    }

    let (topic, qos) = prompt_topic_and_qos(&mut cmd)?;

    let message = get_message_content(&mut cmd)
        .await
        .context("Failed to get message content")?;

    let broker_url = cmd
        .url
        .take()
        .unwrap_or_else(|| format!("mqtt://{}:{}", cmd.host, cmd.port));
    debug!("Connecting to broker: {}", broker_url);

    let client_id = cmd
        .client_id
        .take()
        .unwrap_or_else(|| format!("mqttv5-pub-{}", rand::rng().random::<u32>()));
    let client = MqttClient::new(&client_id);

    #[cfg(feature = "opentelemetry")]
    if let Some(endpoint) = &cmd.otel_endpoint {
        use mqtt5::telemetry::TelemetryConfig;
        let telemetry_config = TelemetryConfig::new(&cmd.otel_service_name)
            .with_endpoint(endpoint)
            .with_sampling_ratio(cmd.otel_sampling);
        mqtt5::telemetry::init_tracing_subscriber(&telemetry_config)?;
        info!(
            "OpenTelemetry enabled: endpoint={}, service={}, sampling={}",
            endpoint, cmd.otel_service_name, cmd.otel_sampling
        );
    }

    let mut options = build_connect_options(&cmd, &client_id);
    configure_auth(&client, &mut options, &cmd).await?;
    configure_will(&mut options, &cmd);

    #[cfg(feature = "codec")]
    configure_codec(&mut options, &cmd)?;

    if cmd.insecure {
        client.set_insecure_tls(true).await;
        info!("Insecure TLS mode enabled (certificate verification disabled)");
    }

    configure_quic_transport(&client, &cmd).await;
    configure_tls_certs(&client, &broker_url, &cmd).await?;

    info!("Connecting to {}...", broker_url);
    let result = client
        .connect_with_options(&broker_url, options)
        .await
        .context("Failed to connect to MQTT broker")?;

    if result.session_present {
        println!("✓ Resumed existing session");
    }

    info!("Publishing to topic '{}'...", topic);

    if cmd.topic_alias == Some(0) {
        anyhow::bail!("Topic alias must be between 1 and 65535, got: 0");
    }

    let correlation_data: Option<Vec<u8>> = if cmd.wait_response && cmd.correlation_data.is_none() {
        Some(format!("rr-{}", rand::rng().random::<u64>()).into_bytes())
    } else if let Some(ref hex_data) = cmd.correlation_data {
        Some(hex::decode(hex_data).context("Invalid hex in --correlation-data")?)
    } else {
        None
    };

    let (received_count, done_notify) =
        setup_response_subscription(&client, &cmd, correlation_data.clone()).await?;

    await_scheduled_delay(&cmd).await?;

    publish_loop(
        &client,
        &topic,
        &message,
        &cmd,
        qos,
        correlation_data.as_ref(),
    )
    .await?;

    wait_for_response(&cmd, done_notify, received_count, &client).await?;

    if cmd.keep_alive_after_publish {
        keep_alive_loop(&client, cmd.auto_reconnect).await?;
    }

    client.disconnect().await?;

    Ok(())
}

async fn await_scheduled_delay(cmd: &PubCommand) -> Result<()> {
    if let Some(delay_secs) = cmd.delay {
        info!(
            "Waiting {} before publishing...",
            humantime::format_duration(std::time::Duration::from_secs(delay_secs))
        );
        tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
    } else if let Some(ref at_time) = cmd.at {
        let wait_duration = calculate_wait_until(at_time)?;
        if wait_duration.as_secs() > 0 {
            info!(
                "Scheduled for {}, waiting {}...",
                at_time,
                humantime::format_duration(wait_duration)
            );
            tokio::time::sleep(wait_duration).await;
        }
    }
    Ok(())
}

#[cfg(feature = "codec")]
fn configure_codec(options: &mut ConnectOptions, cmd: &PubCommand) -> Result<()> {
    if let Some(ref codec_name) = cmd.codec {
        let registry = CodecRegistry::new();
        match codec_name.as_str() {
            "gzip" => {
                registry.register(
                    GzipCodec::new()
                        .with_level(cmd.codec_level)
                        .with_min_size(cmd.codec_min_size),
                );
                registry.set_default("application/gzip")?;
                debug!(
                    "Gzip codec enabled: level={}, min_size={}",
                    cmd.codec_level, cmd.codec_min_size
                );
            }
            "deflate" => {
                registry.register(
                    DeflateCodec::new()
                        .with_level(cmd.codec_level)
                        .with_min_size(cmd.codec_min_size),
                );
                registry.set_default("application/x-deflate")?;
                debug!(
                    "Deflate codec enabled: level={}, min_size={}",
                    cmd.codec_level, cmd.codec_min_size
                );
            }
            _ => anyhow::bail!("Unknown codec: {codec_name}"),
        }
        *options = std::mem::take(options).with_codec_registry(Arc::new(registry));
    }
    Ok(())
}

async fn get_message_content(cmd: &mut PubCommand) -> Result<String> {
    if cmd.stdin {
        debug!("Reading message from stdin");
        let mut buffer = String::new();
        io::stdin()
            .read_to_string(&mut buffer)
            .context("Failed to read from stdin")?;
        return Ok(buffer.trim().to_string());
    }

    if let Some(file_path) = &cmd.file {
        debug!("Reading message from file: {}", file_path);
        let content = tokio::fs::read_to_string(file_path)
            .await
            .with_context(|| format!("Failed to read file: {file_path}"))?;
        return Ok(content.trim().to_string());
    }

    if let Some(message) = &cmd.message {
        return Ok(message.clone());
    }

    if !cmd.non_interactive {
        let message = Input::<String>::new()
            .with_prompt("Message content")
            .interact()
            .context("Failed to get message input")?;
        return Ok(message);
    }

    anyhow::bail!("Message content is required. Use one of:\n  --message \"your message\"\n  --file message.txt\n  --stdin\n  Or run without --non-interactive to be prompted");
}

use rand::Rng;
