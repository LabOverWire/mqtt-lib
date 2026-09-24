use clap::Args;
use mqtt5::{ProtocolVersion, QoS};
use std::path::PathBuf;

use super::parsers::{parse_duration_secs, parse_stream_strategy};

pub fn parse_qos(s: &str) -> Result<QoS, String> {
    match s {
        "0" => Ok(QoS::AtMostOnce),
        "1" => Ok(QoS::AtLeastOnce),
        "2" => Ok(QoS::ExactlyOnce),
        _ => Err(format!("QoS must be 0, 1, or 2, got: {s}")),
    }
}

pub fn parse_protocol_version(s: &str) -> Result<ProtocolVersion, String> {
    match s {
        "3.1.1" | "311" | "4" => Ok(ProtocolVersion::V311),
        "5" | "5.0" => Ok(ProtocolVersion::V5),
        _ => Err(format!("Invalid protocol version: {s}. Use '3.1.1' or '5'")),
    }
}

#[derive(Args)]
pub struct SessionArgs {
    /// Don't clean start (resume existing session)
    #[arg(long = "no-clean-start", env = "MQTT5_NO_CLEAN_START")]
    pub no_clean_start: bool,

    /// Session expiry interval (e.g., 1h, 30m) (0 = expire on disconnect)
    #[arg(long, value_parser = parse_duration_secs, env = "MQTT5_SESSION_EXPIRY")]
    pub session_expiry: Option<u64>,

    /// Keep alive interval (e.g., 60s, 1m) (default: 60s)
    #[arg(long, short = 'k', default_value = "60", value_parser = parse_duration_secs, env = "MQTT5_KEEP_ALIVE")]
    pub keep_alive: u64,

    /// MQTT protocol version (3.1.1 or 5, default: 5)
    #[arg(long, value_parser = parse_protocol_version, env = "MQTT5_PROTOCOL_VERSION")]
    pub protocol_version: Option<ProtocolVersion>,
}

#[derive(Args)]
pub struct WillArgs {
    /// Will topic (last will and testament)
    #[arg(
        id = "will_topic",
        long = "will-topic",
        value_name = "WILL_TOPIC",
        env = "MQTT5_WILL_TOPIC"
    )]
    pub topic: Option<String>,

    /// Will message payload
    #[arg(
        id = "will_message",
        long = "will-message",
        value_name = "WILL_MESSAGE",
        env = "MQTT5_WILL_MESSAGE"
    )]
    pub message: Option<String>,

    /// Will `QoS` level (0, 1, or 2)
    #[arg(id = "will_qos", long = "will-qos", value_name = "WILL_QOS", value_parser = parse_qos, env = "MQTT5_WILL_QOS")]
    pub qos: Option<QoS>,

    /// Will retain flag
    #[arg(id = "will_retain", long = "will-retain", env = "MQTT5_WILL_RETAIN")]
    pub retain: bool,
}

#[derive(Args)]
pub struct TlsArgs {
    /// TLS certificate file (PEM format) for secure connections
    #[arg(long, env = "MQTT5_CERT")]
    pub cert: Option<PathBuf>,

    /// TLS private key file (PEM format) for secure connections
    #[arg(long, env = "MQTT5_KEY")]
    pub key: Option<PathBuf>,

    /// TLS CA certificate file (PEM format) for server verification
    #[arg(long, env = "MQTT5_CA_CERT")]
    pub ca_cert: Option<PathBuf>,

    /// Skip certificate verification for TLS/QUIC connections (insecure, for testing only)
    #[arg(long, env = "MQTT5_INSECURE")]
    pub insecure: bool,
}

#[derive(Args)]
pub struct QuicArgs {
    /// QUIC stream strategy (control-only, per-publish, per-topic, per-subscription)
    #[arg(id = "quic_stream_strategy", long = "quic-stream-strategy", value_name = "QUIC_STREAM_STRATEGY", value_parser = parse_stream_strategy, env = "MQTT5_QUIC_STREAM_STRATEGY")]
    pub stream_strategy: Option<mqtt5::transport::StreamStrategy>,

    /// Enable `MQoQ` flow headers for stream state tracking
    #[arg(
        id = "quic_flow_headers",
        long = "quic-flow-headers",
        env = "MQTT5_QUIC_FLOW_HEADERS"
    )]
    pub flow_headers: bool,

    /// Flow expiration interval (e.g., 5m, 1h) (default: 5m)
    #[arg(id = "quic_flow_expire", long = "quic-flow-expire", value_name = "QUIC_FLOW_EXPIRE", default_value = "300", value_parser = parse_duration_secs, env = "MQTT5_QUIC_FLOW_EXPIRE")]
    pub flow_expire: u64,

    /// Maximum concurrent QUIC streams
    #[arg(
        id = "quic_max_streams",
        long = "quic-max-streams",
        value_name = "QUIC_MAX_STREAMS",
        env = "MQTT5_QUIC_MAX_STREAMS"
    )]
    pub max_streams: Option<usize>,

    /// Enable QUIC datagrams for unreliable transport
    #[arg(
        id = "quic_datagrams",
        long = "quic-datagrams",
        env = "MQTT5_QUIC_DATAGRAMS"
    )]
    pub datagrams: bool,

    /// QUIC connection timeout (e.g., 30s, 1m) (default: 30s)
    #[arg(id = "quic_connect_timeout", long = "quic-connect-timeout", value_name = "QUIC_CONNECT_TIMEOUT", default_value = "30", value_parser = parse_duration_secs, env = "MQTT5_QUIC_CONNECT_TIMEOUT")]
    pub connect_timeout: u64,

    /// Enable QUIC 0-RTT early data for faster reconnections
    #[arg(
        id = "quic_early_data",
        long = "quic-early-data",
        env = "MQTT5_QUIC_EARLY_DATA"
    )]
    pub early_data: bool,
}
