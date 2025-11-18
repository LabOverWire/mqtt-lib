use crate::protocol::v5::reason_codes::ReasonCode;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, MqttError>;

/// MQTT protocol errors
///
/// This enum provides specific error variants for all possible MQTT protocol
/// errors, validation failures, and operational issues.
///
/// # Error Categories
///
/// - **I/O and Network**: `Io`, `ConnectionError`, `Timeout`, `NotConnected`
/// - **Validation**: `InvalidTopicName`, `InvalidTopicFilter`, `InvalidClientId`
/// - **Protocol**: `ProtocolError`, `MalformedPacket`, `InvalidPacketType`
/// - **Authentication**: `AuthenticationFailed`, `NotAuthorized`, `BadUsernameOrPassword`
/// - **Operations**: `SubscriptionFailed`, `PublishFailed`, `UnsubscriptionFailed`
/// - **Server Status**: `ServerUnavailable`, `ServerBusy`, `ServerShuttingDown`
/// - **Flow Control**: `ReceiveMaximumExceeded`, `FlowControlExceeded`, `PacketIdExhausted`
///
/// # Examples
///
/// ```
/// use mqtt5::{MqttError, Result};
///
/// fn validate_topic(topic: &str) -> Result<()> {
///     if topic.contains('#') && !topic.ends_with('#') {
///         return Err(MqttError::InvalidTopicName(
///             "# wildcard must be at the end".to_string()
///         ));
///     }
///     Ok(())
/// }
/// ```
#[derive(Error, Debug, Clone)]
pub enum MqttError {
    #[error("IO error: {0}")]
    Io(String),

    #[error("Invalid topic name: {0}")]
    InvalidTopicName(String),

    #[error("Invalid topic filter: {0}")]
    InvalidTopicFilter(String),

    #[error("Invalid client ID: {0}")]
    InvalidClientId(String),

    #[error("Connection error: {0}")]
    ConnectionError(String),

    #[error("Connection refused: {0:?}")]
    ConnectionRefused(ReasonCode),

    #[error("Protocol error: {0}")]
    ProtocolError(String),

    #[error("Malformed packet: {0}")]
    MalformedPacket(String),

    #[error("Packet too large: size {size} exceeds maximum {max}")]
    PacketTooLarge { size: usize, max: usize },

    #[error("Authentication failed")]
    AuthenticationFailed,

    #[error("Not authorized")]
    NotAuthorized,

    #[error("Not connected")]
    NotConnected,

    #[error("Already connected")]
    AlreadyConnected,

    #[error("Timeout")]
    Timeout,

    #[error("Subscription failed: {0:?}")]
    SubscriptionFailed(ReasonCode),

    #[error("Unsubscription failed: {0:?}")]
    UnsubscriptionFailed(ReasonCode),

    #[error("Publish failed: {0:?}")]
    PublishFailed(ReasonCode),

    #[error("Packet identifier not found: {0}")]
    PacketIdNotFound(u16),

    #[error("Packet identifier already in use: {0}")]
    PacketIdInUse(u16),

    #[error("Invalid QoS: {0}")]
    InvalidQoS(u8),

    #[error("Invalid packet type: {0}")]
    InvalidPacketType(u8),

    #[error("Invalid reason code: {0}")]
    InvalidReasonCode(u8),

    #[error("Invalid property ID: {0}")]
    InvalidPropertyId(u8),

    #[error("Duplicate property ID: {0}")]
    DuplicatePropertyId(u8),

    #[error("Session expired")]
    SessionExpired,

    #[error("Keep alive timeout")]
    KeepAliveTimeout,

    #[error("Server shutting down")]
    ServerShuttingDown,

    #[error("Client closed connection")]
    ClientClosed,

    #[error("Connection closed by peer")]
    ConnectionClosedByPeer,

    #[error("Maximum connect time exceeded")]
    MaxConnectTime,

    #[error("Topic alias invalid: {0}")]
    TopicAliasInvalid(u16),

    #[error("Receive maximum exceeded")]
    ReceiveMaximumExceeded,

    #[error("Will message rejected")]
    WillRejected,

    #[error("Implementation specific error: {0}")]
    ImplementationSpecific(String),

    #[error("Unsupported protocol version")]
    UnsupportedProtocolVersion,

    #[error("Invalid state: {0}")]
    InvalidState(String),

    #[error("Client identifier not valid")]
    ClientIdentifierNotValid,

    #[error("Bad username or password")]
    BadUsernameOrPassword,

    #[error("Server unavailable")]
    ServerUnavailable,

    #[error("Server busy")]
    ServerBusy,

    #[error("Banned")]
    Banned,

    #[error("Bad authentication method")]
    BadAuthenticationMethod,

    #[error("Quota exceeded")]
    QuotaExceeded,

    #[error("Payload format invalid")]
    PayloadFormatInvalid,

    #[error("Retain not supported")]
    RetainNotSupported,

    #[error("QoS not supported")]
    QoSNotSupported,

    #[error("Use another server")]
    UseAnotherServer,

    #[error("Server moved")]
    ServerMoved,

    #[error("Shared subscriptions not supported")]
    SharedSubscriptionsNotSupported,

    #[error("Connection rate exceeded")]
    ConnectionRateExceeded,

    #[error("Subscription identifiers not supported")]
    SubscriptionIdentifiersNotSupported,

    #[error("Wildcard subscriptions not supported")]
    WildcardSubscriptionsNotSupported,

    #[error("Message too large for queue")]
    MessageTooLarge,

    #[error("Flow control exceeded")]
    FlowControlExceeded,

    #[error("Packet ID exhausted")]
    PacketIdExhausted,

    #[error("String too long: {0} bytes exceeds maximum of 65535")]
    StringTooLong(usize),

    #[error("Configuration error: {0}")]
    Configuration(String),
}

impl MqttError {
    pub fn is_normal_disconnect(&self) -> bool {
        match self {
            Self::ClientClosed | Self::ConnectionClosedByPeer => true,
            Self::Io(msg)
                if msg.contains("stream has been shut down")
                    || msg.contains("Connection reset") =>
            {
                true
            }
            _ => false,
        }
    }
}

impl From<std::io::Error> for MqttError {
    fn from(err: std::io::Error) -> Self {
        MqttError::Io(err.to_string())
    }
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for MqttError {
    fn from(err: tokio::sync::mpsc::error::SendError<T>) -> Self {
        MqttError::ConnectionError(format!("Channel send error: {err}"))
    }
}

// Error conversions for BeBytes compatibility
impl From<String> for MqttError {
    fn from(msg: String) -> Self {
        MqttError::MalformedPacket(msg)
    }
}

impl From<&str> for MqttError {
    fn from(msg: &str) -> Self {
        MqttError::MalformedPacket(msg.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn test_error_display() {
        let err = MqttError::InvalidTopicName("test/+/topic".to_string());
        assert_eq!(err.to_string(), "Invalid topic name: test/+/topic");

        let err = MqttError::PacketTooLarge {
            size: 1000,
            max: 500,
        };
        assert_eq!(
            err.to_string(),
            "Packet too large: size 1000 exceeds maximum 500"
        );

        let err = MqttError::ConnectionRefused(ReasonCode::BadUsernameOrPassword);
        assert_eq!(err.to_string(), "Connection refused: BadUsernameOrPassword");
    }

    #[test]
    fn test_error_from_io() {
        let io_err = io::Error::new(io::ErrorKind::ConnectionRefused, "test");
        let mqtt_err: MqttError = io_err.into();
        match mqtt_err {
            MqttError::Io(e) => assert!(e.contains("test")),
            _ => panic!("Expected Io error"),
        }
    }

    #[test]
    fn test_result_type() {
        #[allow(clippy::unnecessary_wraps)]
        fn returns_result() -> Result<String> {
            Ok("success".to_string())
        }

        fn returns_error() -> Result<String> {
            Err(MqttError::NotConnected)
        }

        assert!(returns_result().is_ok());
        assert!(returns_error().is_err());
    }
}
