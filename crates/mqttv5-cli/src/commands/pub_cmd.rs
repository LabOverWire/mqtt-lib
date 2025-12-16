use anyhow::{Context, Result};
use clap::Args;
use dialoguer::{Input, Select};
use mqtt5::time::Duration;
use mqtt5::{
    ConnectOptions, ConnectionEvent, MqttClient, ProtocolVersion, PublishOptions, QoS, WillMessage,
};
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use tokio::signal;
use tracing::{debug, info, warn};

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

    /// MQTT broker URL (mqtt://, mqtts://, ws://, wss://, quic://, quics://)
    #[arg(long, short = 'U', conflicts_with_all = &["host", "port"])]
    pub url: Option<String>,

    /// MQTT broker host
    #[arg(long, short = 'H', default_value = "localhost")]
    pub host: String,

    /// MQTT broker port
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

    /// Session expiry interval in seconds (0 = expire on disconnect)
    #[arg(long)]
    pub session_expiry: Option<u32>,

    /// Keep alive interval in seconds
    #[arg(long, short = 'k', default_value = "60")]
    pub keep_alive: u16,

    /// MQTT protocol version (3.1.1 or 5, default: 5)
    #[arg(long, value_parser = parse_protocol_version)]
    pub protocol_version: Option<ProtocolVersion>,

    /// Will topic (last will and testament)
    #[arg(long)]
    pub will_topic: Option<String>,

    /// Will message payload
    #[arg(long)]
    pub will_message: Option<String>,

    /// Will QoS level (0, 1, or 2)
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

    /// Will delay interval in seconds
    #[arg(long)]
    pub will_delay: Option<u32>,

    /// Keep connection alive after publishing (for testing will messages)
    #[arg(long, hide = true)]
    pub keep_alive_after_publish: bool,

    /// Enable automatic reconnection when broker disconnects
    #[arg(long)]
    pub auto_reconnect: bool,

    /// QUIC stream strategy (control-only, per-publish, per-topic, per-subscription)
    #[arg(long, value_parser = parse_stream_strategy)]
    pub quic_stream_strategy: Option<mqtt5::transport::StreamStrategy>,

    /// Enable MQoQ flow headers for stream state tracking
    #[arg(long)]
    pub quic_flow_headers: bool,

    /// Flow expiration interval in seconds (default: 300)
    #[arg(long, default_value = "300")]
    pub quic_flow_expire: u64,

    /// Maximum concurrent QUIC streams
    #[arg(long)]
    pub quic_max_streams: Option<usize>,

    /// Enable QUIC datagrams for unreliable transport
    #[arg(long)]
    pub quic_datagrams: bool,

    /// QUIC connection timeout in seconds (default: 30)
    #[arg(long, default_value = "30")]
    pub quic_connect_timeout: u64,

    /// OpenTelemetry OTLP endpoint (e.g., http://localhost:4317)
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

pub async fn execute(mut cmd: PubCommand, verbose: bool, debug: bool) -> Result<()> {
    #[cfg(feature = "opentelemetry")]
    let has_otel = cmd.otel_endpoint.is_some();

    #[cfg(not(feature = "opentelemetry"))]
    let has_otel = false;

    if !has_otel {
        crate::init_basic_tracing(verbose, debug);
    }

    // Smart prompting for missing required arguments
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

    // Validate topic
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

    // Get message content with smart prompting
    let message = get_message_content(&mut cmd)
        .await
        .context("Failed to get message content")?;

    // Smart QoS prompting
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
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => QoS::AtMostOnce,
        }
    } else {
        cmd.qos.unwrap_or(QoS::AtMostOnce)
    };

    // Build broker URL
    let broker_url = cmd
        .url
        .unwrap_or_else(|| format!("mqtt://{}:{}", cmd.host, cmd.port));
    debug!("Connecting to broker: {}", broker_url);

    // Create client
    let client_id = cmd
        .client_id
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

    // Build connection options
    let mut options = ConnectOptions::new(client_id.clone())
        .with_clean_start(!cmd.no_clean_start)
        .with_keep_alive(Duration::from_secs(cmd.keep_alive.into()));

    if cmd.auto_reconnect {
        options = options.with_automatic_reconnect(true);
    }

    if let Some(version) = cmd.protocol_version {
        options = options.with_protocol_version(version);
    }

    // Add session expiry if specified
    if let Some(expiry) = cmd.session_expiry {
        options = options.with_session_expiry_interval(expiry);
    }

    // Add authentication based on method
    match cmd.auth_method.as_deref() {
        Some("scram") => {
            let username = cmd.username.clone().ok_or_else(|| {
                anyhow::anyhow!("--username is required for SCRAM authentication")
            })?;
            let password = cmd.password.clone().ok_or_else(|| {
                anyhow::anyhow!("--password is required for SCRAM authentication")
            })?;
            options = options
                .with_credentials(username.clone(), Vec::new())
                .with_authentication_method("SCRAM-SHA-256");

            use mqtt5::client::auth_handlers::ScramSha256AuthHandler;
            let handler = ScramSha256AuthHandler::new(username, password);
            client.set_auth_handler(handler).await;
            debug!("SCRAM-SHA-256 authentication configured");
        }
        Some("jwt") => {
            let token = cmd
                .jwt_token
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--jwt-token is required for JWT authentication"))?;
            options = options.with_authentication_method("JWT");

            use mqtt5::client::auth_handlers::JwtAuthHandler;
            let handler = JwtAuthHandler::new(token);
            client.set_auth_handler(handler).await;
            debug!("JWT authentication configured");
        }
        Some("password") | None => {
            if let (Some(username), Some(password)) = (cmd.username.clone(), cmd.password.clone()) {
                options = options.with_credentials(username, password.into_bytes());
            } else if let Some(username) = cmd.username.clone() {
                options = options.with_credentials(username, Vec::new());
            }
        }
        Some(other) => anyhow::bail!("Unknown auth method: {other}"),
    }

    // Add will message if specified
    if let Some(topic) = cmd.will_topic.clone() {
        let payload = cmd.will_message.clone().unwrap_or_default();
        let mut will = WillMessage::new(topic, payload.into_bytes()).with_retain(cmd.will_retain);

        if let Some(qos) = cmd.will_qos {
            will = will.with_qos(qos);
        }

        if let Some(delay) = cmd.will_delay {
            will.properties.will_delay_interval = Some(delay);
        }

        options = options.with_will(will);
    }

    // Configure insecure TLS mode if requested
    if cmd.insecure {
        client.set_insecure_tls(true).await;
        info!("Insecure TLS mode enabled (certificate verification disabled)");
    }

    // Configure QUIC transport options
    if let Some(strategy) = cmd.quic_stream_strategy {
        client.set_quic_stream_strategy(strategy).await;
        debug!("QUIC stream strategy: {:?}", strategy);
    }
    if cmd.quic_flow_headers {
        client.set_quic_flow_headers(true).await;
        debug!("QUIC flow headers enabled");
    }
    client.set_quic_flow_expire(cmd.quic_flow_expire).await;
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

    // Configure TLS if using secure connection
    if broker_url.starts_with("ssl://") || broker_url.starts_with("mqtts://") {
        // Configure with certificates if provided
        if cmd.cert.is_some() || cmd.key.is_some() || cmd.ca_cert.is_some() {
            let cert_pem =
                if let Some(cert_path) = &cmd.cert {
                    Some(std::fs::read(cert_path).with_context(|| {
                        format!("Failed to read certificate file: {cert_path:?}")
                    })?)
                } else {
                    None
                };
            let key_pem = if let Some(key_path) = &cmd.key {
                Some(
                    std::fs::read(key_path)
                        .with_context(|| format!("Failed to read key file: {key_path:?}"))?,
                )
            } else {
                None
            };
            let ca_pem =
                if let Some(ca_path) = &cmd.ca_cert {
                    Some(std::fs::read(ca_path).with_context(|| {
                        format!("Failed to read CA certificate file: {ca_path:?}")
                    })?)
                } else {
                    None
                };

            client.set_tls_config(cert_pem, key_pem, ca_pem).await;
        }
    }

    // Connect
    info!("Connecting to {}...", broker_url);
    let result = client
        .connect_with_options(&broker_url, options)
        .await
        .context("Failed to connect to MQTT broker")?;

    if result.session_present {
        println!("✓ Resumed existing session");
    }

    // Publish message
    info!("Publishing to topic '{}'...", topic);

    if cmd.topic_alias == Some(0) {
        anyhow::bail!("Topic alias must be between 1 and 65535, got: 0");
    }

    let has_properties =
        cmd.retain || cmd.message_expiry_interval.is_some() || cmd.topic_alias.is_some();

    if has_properties {
        let mut options = PublishOptions {
            qos,
            retain: cmd.retain,
            ..Default::default()
        };
        options.properties.message_expiry_interval = cmd.message_expiry_interval;
        options.properties.topic_alias = cmd.topic_alias;
        client
            .publish_with_options(&topic, message.as_bytes(), options)
            .await?;
    } else {
        match qos {
            QoS::AtMostOnce => {
                client.publish(&topic, message.as_bytes()).await?;
            }
            QoS::AtLeastOnce => {
                client.publish_qos1(&topic, message.as_bytes()).await?;
            }
            QoS::ExactlyOnce => {
                client.publish_qos2(&topic, message.as_bytes()).await?;
            }
        }
    }

    println!("✓ Published message to '{}' (QoS {})", topic, qos as u8);
    if cmd.retain {
        println!("  Message retained on broker");
    }

    // Keep connection alive if requested (for testing will messages)
    if cmd.keep_alive_after_publish {
        info!("Keeping connection alive (--keep-alive-after-publish)");

        if cmd.auto_reconnect {
            client
                .on_connection_event(move |event| match event {
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
                        warn!("⚠ Reconnection failed: {}", error);
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
    }

    // Disconnect
    client.disconnect().await?;

    Ok(())
}

async fn get_message_content(cmd: &mut PubCommand) -> Result<String> {
    // Priority: stdin > file > message > prompt
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
        return fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {file_path}"))
            .map(|s| s.trim().to_string());
    }

    if let Some(message) = &cmd.message {
        return Ok(message.clone());
    }

    // Smart prompting for message
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
