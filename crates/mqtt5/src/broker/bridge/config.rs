//! Bridge configuration types

use crate::time::Duration;
use crate::transport::StreamStrategy;
use crate::QoS;
pub use mqtt5_protocol::BridgeDirection;
use serde::{Deserialize, Serialize};

/// Bridge connection configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct BridgeConfig {
    /// Unique name for this bridge
    pub name: String,

    /// Remote broker address (hostname:port)
    pub remote_address: String,

    /// Client ID to use when connecting to remote broker
    pub client_id: String,

    /// Username for authentication (optional)
    pub username: Option<String>,

    /// Password for authentication (optional)
    pub password: Option<String>,

    /// Transport protocol to use for the bridge connection
    #[serde(default)]
    pub protocol: BridgeProtocol,

    /// Whether to use TLS (optional, deprecated: use `protocol` instead)
    #[serde(default)]
    pub use_tls: bool,

    /// TLS server name for verification (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_server_name: Option<String>,

    /// Path to CA certificate file for TLS verification (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ca_file: Option<String>,

    /// Path to client certificate file for mTLS (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_cert_file: Option<String>,

    /// Path to client private key file for mTLS (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_key_file: Option<String>,

    /// Disable TLS certificate verification (for testing only)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insecure: Option<bool>,

    /// QUIC stream strategy (only used when protocol is quic or quics)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quic_stream_strategy: Option<StreamStrategy>,

    /// Enable `MQoQ` flow headers for QUIC connections
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quic_flow_headers: Option<bool>,

    /// Enable QUIC datagrams for `QoS` 0 messages
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quic_datagrams: Option<bool>,

    /// Maximum concurrent QUIC streams
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quic_max_streams: Option<usize>,

    /// Fallback protocols to try if primary protocol fails
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_protocols: Vec<BridgeProtocol>,

    /// Convenience flag: fall back to TCP if primary protocol fails
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback_tcp: bool,

    /// ALPN protocols (e.g., `["x-amzn-mqtt-ca"]` for AWS `IoT`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpn_protocols: Option<Vec<String>>,

    /// Indicate this is a bridge connection (mosquitto compatibility)
    #[serde(default = "default_try_private")]
    pub try_private: bool,

    /// Whether to start with a clean session
    #[serde(default = "default_clean_start")]
    pub clean_start: bool,

    /// Keep-alive interval in seconds
    #[serde(default = "default_keepalive")]
    pub keepalive: u16,

    /// MQTT protocol version
    #[serde(default)]
    pub protocol_version: MqttVersion,

    /// Delay between reconnection attempts (deprecated: use `initial_reconnect_delay`)
    #[serde(with = "humantime_serde", default = "default_reconnect_delay")]
    pub reconnect_delay: Duration,

    /// Initial delay for first reconnection attempt
    #[serde(with = "humantime_serde", default = "default_reconnect_delay")]
    pub initial_reconnect_delay: Duration,

    /// Maximum delay between reconnection attempts
    #[serde(with = "humantime_serde", default = "default_max_reconnect_delay")]
    pub max_reconnect_delay: Duration,

    /// Multiplier for exponential backoff
    #[serde(default = "default_backoff_multiplier")]
    pub backoff_multiplier: f64,

    /// Maximum reconnection attempts (None = infinite)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_reconnect_attempts: Option<u32>,

    /// Number of connection retries per broker before trying fallback protocol
    #[serde(default = "default_connection_retries")]
    pub connection_retries: u8,

    /// Delay before first retry attempt (quick retry)
    #[serde(with = "humantime_serde", default = "default_first_retry_delay")]
    pub first_retry_delay: Duration,

    /// Enable jitter on retry delays (±25%) to prevent thundering herd
    #[serde(default = "default_retry_jitter")]
    pub retry_jitter: bool,

    /// Backup broker addresses for failover
    #[serde(default)]
    pub backup_brokers: Vec<String>,

    /// Interval for health-checking primary broker when connected to backup
    #[serde(
        with = "humantime_serde",
        default = "default_primary_health_check_interval"
    )]
    pub primary_health_check_interval: Duration,

    /// Whether to automatically failback to primary when it becomes available
    #[serde(default = "default_enable_failback")]
    pub enable_failback: bool,

    /// Topic mappings for this bridge
    #[serde(default)]
    pub topics: Vec<TopicMapping>,
}

/// Topic mapping configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicMapping {
    /// Topic pattern (can include MQTT wildcards)
    pub pattern: String,

    /// Direction of message flow
    pub direction: BridgeDirection,

    /// Quality of Service level
    #[serde(default = "default_qos")]
    pub qos: QoS,

    /// Prefix to add to local topics (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_prefix: Option<String>,

    /// Prefix to add to remote topics (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_prefix: Option<String>,
}

/// Transport protocol for bridge connections
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BridgeProtocol {
    /// Plain TCP (mqtt://)
    #[default]
    Tcp,
    /// TLS over TCP (mqtts://)
    Tls,
    /// QUIC without certificate verification (quic://)
    Quic,
    /// QUIC with certificate verification (quics://)
    QuicSecure,
}

/// MQTT protocol version
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MqttVersion {
    /// MQTT 3.1
    #[serde(rename = "mqttv31")]
    V31,
    /// MQTT 3.1.1
    #[serde(rename = "mqttv311")]
    V311,
    /// MQTT 5.0
    #[serde(rename = "mqttv50")]
    #[default]
    V50,
}

// Default value functions for serde
fn default_try_private() -> bool {
    true
}

fn default_clean_start() -> bool {
    false
}

fn default_keepalive() -> u16 {
    60
}

fn default_reconnect_delay() -> Duration {
    Duration::from_secs(5)
}

fn default_max_reconnect_delay() -> Duration {
    Duration::from_secs(300)
}

fn default_backoff_multiplier() -> f64 {
    2.0
}

fn default_qos() -> QoS {
    QoS::AtMostOnce
}

fn default_primary_health_check_interval() -> Duration {
    Duration::from_secs(30)
}

fn default_enable_failback() -> bool {
    true
}

fn default_connection_retries() -> u8 {
    3
}

fn default_first_retry_delay() -> Duration {
    Duration::from_secs(1)
}

fn default_retry_jitter() -> bool {
    true
}

impl BridgeConfig {
    /// Creates a new bridge configuration
    pub fn new(name: impl Into<String>, remote_address: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            name: name.clone(),
            remote_address: remote_address.into(),
            client_id: format!("bridge-{name}"),
            username: None,
            password: None,
            protocol: BridgeProtocol::Tcp,
            use_tls: false,
            tls_server_name: None,
            ca_file: None,
            client_cert_file: None,
            client_key_file: None,
            insecure: None,
            quic_stream_strategy: None,
            quic_flow_headers: None,
            quic_datagrams: None,
            quic_max_streams: None,
            fallback_protocols: Vec::new(),
            fallback_tcp: false,
            alpn_protocols: None,
            try_private: true,
            clean_start: false,
            keepalive: 60,
            protocol_version: MqttVersion::V50,
            reconnect_delay: Duration::from_secs(5),
            initial_reconnect_delay: Duration::from_secs(5),
            max_reconnect_delay: Duration::from_secs(300),
            backoff_multiplier: 2.0,
            max_reconnect_attempts: None,
            connection_retries: 3,
            first_retry_delay: Duration::from_secs(1),
            retry_jitter: true,
            backup_brokers: Vec::new(),
            primary_health_check_interval: Duration::from_secs(30),
            enable_failback: true,
            topics: Vec::new(),
        }
    }

    /// Adds a topic mapping to the bridge
    #[must_use]
    pub fn add_topic(
        mut self,
        pattern: impl Into<String>,
        direction: BridgeDirection,
        qos: QoS,
    ) -> Self {
        self.topics.push(TopicMapping {
            pattern: pattern.into(),
            direction,
            qos,
            local_prefix: None,
            remote_prefix: None,
        });
        self
    }

    /// Adds a backup broker address
    #[must_use]
    pub fn add_backup_broker(mut self, address: impl Into<String>) -> Self {
        self.backup_brokers.push(address.into());
        self
    }

    /// Validates the configuration.
    ///
    /// # Errors
    /// Returns an error if the configuration is invalid (empty name, client ID, or topics).
    pub fn validate(&self) -> crate::Result<()> {
        use crate::error::MqttError;

        if self.name.is_empty() {
            return Err(MqttError::Configuration(
                "Bridge name cannot be empty".into(),
            ));
        }

        if self.client_id.is_empty() {
            return Err(MqttError::Configuration("Client ID cannot be empty".into()));
        }

        if self.topics.is_empty() {
            return Err(MqttError::Configuration(
                "Bridge must have at least one topic mapping".into(),
            ));
        }

        // Validate topic patterns
        for topic in &self.topics {
            if topic.pattern.is_empty() {
                return Err(MqttError::Configuration(
                    "Topic pattern cannot be empty".into(),
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_config_creation() {
        let config = BridgeConfig::new("test-bridge", "remote.broker:1883")
            .add_topic("sensors/+/data", BridgeDirection::Out, QoS::AtLeastOnce)
            .add_backup_broker("backup.broker:1883");

        assert_eq!(config.name, "test-bridge");
        assert_eq!(config.client_id, "bridge-test-bridge");
        assert_eq!(config.topics.len(), 1);
        assert_eq!(config.backup_brokers.len(), 1);
    }

    #[test]
    fn test_bridge_config_validation() {
        let mut config = BridgeConfig::new("test", "broker:1883");

        // Should fail without topics
        assert!(config.validate().is_err());

        // Should pass with topics
        config = config.add_topic("test/#", BridgeDirection::Both, QoS::AtMostOnce);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_try_private_serialization() {
        let mut config = BridgeConfig::new("test-bridge", "remote.broker:1883").add_topic(
            "test/#",
            BridgeDirection::Both,
            QoS::AtMostOnce,
        );
        config.try_private = true;

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"try_private\":true"));

        let deserialized: BridgeConfig = serde_json::from_str(&json).unwrap();
        assert!(deserialized.try_private);
        assert_eq!(deserialized.name, "test-bridge");
    }

    #[test]
    fn test_try_private_default_value() {
        let config = BridgeConfig::new("test-bridge", "remote.broker:1883").add_topic(
            "test/#",
            BridgeDirection::Both,
            QoS::AtMostOnce,
        );

        assert!(config.try_private);

        let json = r#"{
            "name": "test-bridge",
            "client_id": "bridge-test-bridge",
            "remote_address": "remote.broker:1883",
            "topics": [{
                "pattern": "test/#",
                "direction": "both",
                "qos": "AtMostOnce"
            }]
        }"#;

        let deserialized: BridgeConfig = serde_json::from_str(json).unwrap();
        assert!(deserialized.try_private);
    }

    #[test]
    fn test_try_private_false_serialization() {
        let mut config = BridgeConfig::new("test-bridge", "remote.broker:1883").add_topic(
            "test/#",
            BridgeDirection::Both,
            QoS::AtMostOnce,
        );
        config.try_private = false;

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"try_private\":false"));

        let deserialized: BridgeConfig = serde_json::from_str(&json).unwrap();
        assert!(!deserialized.try_private);
    }
}
