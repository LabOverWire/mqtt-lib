use crate::packet::suback::SubAckReasonCode;
use crate::prelude::String;
use crate::protocol::v5::reason_codes::ReasonCode;

#[cfg(feature = "std")]
use thiserror::Error;

#[cfg(not(feature = "std"))]
use core::fmt;

#[cfg(feature = "std")]
pub type Result<T> = std::result::Result<T, MqttError>;

#[cfg(not(feature = "std"))]
pub type Result<T> = core::result::Result<T, MqttError>;

#[derive(Debug, Clone)]
#[cfg_attr(feature = "std", derive(Error))]
pub enum MqttError {
    #[cfg_attr(feature = "std", error("IO error: {0}"))]
    Io(String),

    #[cfg_attr(feature = "std", error("Invalid topic name: {0}"))]
    InvalidTopicName(String),

    #[cfg_attr(feature = "std", error("Invalid topic filter: {0}"))]
    InvalidTopicFilter(String),

    #[cfg_attr(feature = "std", error("Invalid client ID: {0}"))]
    InvalidClientId(String),

    #[cfg_attr(feature = "std", error("Connection error: {0}"))]
    ConnectionError(String),

    #[cfg_attr(feature = "std", error("Connection refused: {0:?}"))]
    ConnectionRefused(ReasonCode),

    #[cfg_attr(feature = "std", error("Protocol error: {0}"))]
    ProtocolError(String),

    #[cfg_attr(feature = "std", error("Malformed packet: {0}"))]
    MalformedPacket(String),

    #[cfg_attr(
        feature = "std",
        error("Packet too large: size {size} exceeds maximum {max}")
    )]
    PacketTooLarge { size: usize, max: usize },

    #[cfg_attr(feature = "std", error("Authentication failed"))]
    AuthenticationFailed,

    #[cfg_attr(feature = "std", error("Not authorized"))]
    NotAuthorized,

    #[cfg_attr(feature = "std", error("Not connected"))]
    NotConnected,

    #[cfg_attr(feature = "std", error("Already connected"))]
    AlreadyConnected,

    #[cfg_attr(feature = "std", error("Timeout"))]
    Timeout,

    #[cfg_attr(feature = "std", error("Subscription failed: {0:?}"))]
    SubscriptionFailed(ReasonCode),

    #[cfg_attr(feature = "std", error("Subscription denied: {0:?}"))]
    SubscriptionDenied(SubAckReasonCode),

    #[cfg_attr(feature = "std", error("Unsubscription failed: {0:?}"))]
    UnsubscriptionFailed(ReasonCode),

    #[cfg_attr(feature = "std", error("Publish failed: {0:?}"))]
    PublishFailed(ReasonCode),

    #[cfg_attr(feature = "std", error("Packet identifier not found: {0}"))]
    PacketIdNotFound(u16),

    #[cfg_attr(feature = "std", error("Packet identifier already in use: {0}"))]
    PacketIdInUse(u16),

    #[cfg_attr(feature = "std", error("Invalid QoS: {0}"))]
    InvalidQoS(u8),

    #[cfg_attr(feature = "std", error("Invalid packet type: {0}"))]
    InvalidPacketType(u8),

    #[cfg_attr(feature = "std", error("Invalid reason code: {0}"))]
    InvalidReasonCode(u8),

    #[cfg_attr(feature = "std", error("Invalid property ID: {0}"))]
    InvalidPropertyId(u8),

    #[cfg_attr(feature = "std", error("Duplicate property ID: {0}"))]
    DuplicatePropertyId(u8),

    #[cfg_attr(feature = "std", error("Session expired"))]
    SessionExpired,

    #[cfg_attr(feature = "std", error("Keep alive timeout"))]
    KeepAliveTimeout,

    #[cfg_attr(feature = "std", error("Server shutting down"))]
    ServerShuttingDown,

    #[cfg_attr(feature = "std", error("Client closed connection"))]
    ClientClosed,

    #[cfg_attr(feature = "std", error("Connection closed by peer"))]
    ConnectionClosedByPeer,

    #[cfg_attr(feature = "std", error("Maximum connect time exceeded"))]
    MaxConnectTime,

    #[cfg_attr(feature = "std", error("Topic alias invalid: {0}"))]
    TopicAliasInvalid(u16),

    #[cfg_attr(feature = "std", error("Receive maximum exceeded"))]
    ReceiveMaximumExceeded,

    #[cfg_attr(feature = "std", error("Will message rejected"))]
    WillRejected,

    #[cfg_attr(feature = "std", error("Implementation specific error: {0}"))]
    ImplementationSpecific(String),

    #[cfg_attr(feature = "std", error("Unsupported protocol version"))]
    UnsupportedProtocolVersion,

    #[cfg_attr(feature = "std", error("Invalid state: {0}"))]
    InvalidState(String),

    #[cfg_attr(feature = "std", error("Client identifier not valid"))]
    ClientIdentifierNotValid,

    #[cfg_attr(feature = "std", error("Bad username or password"))]
    BadUsernameOrPassword,

    #[cfg_attr(feature = "std", error("Server unavailable"))]
    ServerUnavailable,

    #[cfg_attr(feature = "std", error("Server busy"))]
    ServerBusy,

    #[cfg_attr(feature = "std", error("Banned"))]
    Banned,

    #[cfg_attr(feature = "std", error("Bad authentication method"))]
    BadAuthenticationMethod,

    #[cfg_attr(feature = "std", error("Quota exceeded"))]
    QuotaExceeded,

    #[cfg_attr(feature = "std", error("Payload format invalid"))]
    PayloadFormatInvalid,

    #[cfg_attr(feature = "std", error("Retain not supported"))]
    RetainNotSupported,

    #[cfg_attr(feature = "std", error("QoS not supported"))]
    QoSNotSupported,

    #[cfg_attr(feature = "std", error("Use another server"))]
    UseAnotherServer,

    #[cfg_attr(feature = "std", error("Server moved"))]
    ServerMoved,

    #[cfg_attr(feature = "std", error("Shared subscriptions not supported"))]
    SharedSubscriptionsNotSupported,

    #[cfg_attr(feature = "std", error("Connection rate exceeded"))]
    ConnectionRateExceeded,

    #[cfg_attr(feature = "std", error("Subscription identifiers not supported"))]
    SubscriptionIdentifiersNotSupported,

    #[cfg_attr(feature = "std", error("Wildcard subscriptions not supported"))]
    WildcardSubscriptionsNotSupported,

    #[cfg_attr(feature = "std", error("Message too large for queue"))]
    MessageTooLarge,

    #[cfg_attr(feature = "std", error("Flow control exceeded"))]
    FlowControlExceeded,

    #[cfg_attr(feature = "std", error("Packet ID exhausted"))]
    PacketIdExhausted,

    #[cfg_attr(
        feature = "std",
        error("String too long: {0} bytes exceeds maximum of 65535")
    )]
    StringTooLong(usize),

    #[cfg_attr(feature = "std", error("Configuration error: {0}"))]
    Configuration(String),
}

#[cfg(not(feature = "std"))]
impl fmt::Display for MqttError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(s) => write!(f, "IO error: {s}"),
            Self::InvalidTopicName(s) => write!(f, "Invalid topic name: {s}"),
            Self::InvalidTopicFilter(s) => write!(f, "Invalid topic filter: {s}"),
            Self::InvalidClientId(s) => write!(f, "Invalid client ID: {s}"),
            Self::ConnectionError(s) => write!(f, "Connection error: {s}"),
            Self::ConnectionRefused(r) => write!(f, "Connection refused: {r:?}"),
            Self::ProtocolError(s) => write!(f, "Protocol error: {s}"),
            Self::MalformedPacket(s) => write!(f, "Malformed packet: {s}"),
            Self::PacketTooLarge { size, max } => {
                write!(f, "Packet too large: size {size} exceeds maximum {max}")
            }
            Self::AuthenticationFailed => write!(f, "Authentication failed"),
            Self::NotAuthorized => write!(f, "Not authorized"),
            Self::NotConnected => write!(f, "Not connected"),
            Self::AlreadyConnected => write!(f, "Already connected"),
            Self::Timeout => write!(f, "Timeout"),
            Self::SubscriptionFailed(r) => write!(f, "Subscription failed: {r:?}"),
            Self::SubscriptionDenied(r) => write!(f, "Subscription denied: {r:?}"),
            Self::UnsubscriptionFailed(r) => write!(f, "Unsubscription failed: {r:?}"),
            Self::PublishFailed(r) => write!(f, "Publish failed: {r:?}"),
            Self::PacketIdNotFound(id) => write!(f, "Packet identifier not found: {id}"),
            Self::PacketIdInUse(id) => write!(f, "Packet identifier already in use: {id}"),
            Self::InvalidQoS(q) => write!(f, "Invalid QoS: {q}"),
            Self::InvalidPacketType(t) => write!(f, "Invalid packet type: {t}"),
            Self::InvalidReasonCode(r) => write!(f, "Invalid reason code: {r}"),
            Self::InvalidPropertyId(p) => write!(f, "Invalid property ID: {p}"),
            Self::DuplicatePropertyId(p) => write!(f, "Duplicate property ID: {p}"),
            Self::SessionExpired => write!(f, "Session expired"),
            Self::KeepAliveTimeout => write!(f, "Keep alive timeout"),
            Self::ServerShuttingDown => write!(f, "Server shutting down"),
            Self::ClientClosed => write!(f, "Client closed connection"),
            Self::ConnectionClosedByPeer => write!(f, "Connection closed by peer"),
            Self::MaxConnectTime => write!(f, "Maximum connect time exceeded"),
            Self::TopicAliasInvalid(a) => write!(f, "Topic alias invalid: {a}"),
            Self::ReceiveMaximumExceeded => write!(f, "Receive maximum exceeded"),
            Self::WillRejected => write!(f, "Will message rejected"),
            Self::ImplementationSpecific(s) => write!(f, "Implementation specific error: {s}"),
            Self::UnsupportedProtocolVersion => write!(f, "Unsupported protocol version"),
            Self::InvalidState(s) => write!(f, "Invalid state: {s}"),
            Self::ClientIdentifierNotValid => write!(f, "Client identifier not valid"),
            Self::BadUsernameOrPassword => write!(f, "Bad username or password"),
            Self::ServerUnavailable => write!(f, "Server unavailable"),
            Self::ServerBusy => write!(f, "Server busy"),
            Self::Banned => write!(f, "Banned"),
            Self::BadAuthenticationMethod => write!(f, "Bad authentication method"),
            Self::QuotaExceeded => write!(f, "Quota exceeded"),
            Self::PayloadFormatInvalid => write!(f, "Payload format invalid"),
            Self::RetainNotSupported => write!(f, "Retain not supported"),
            Self::QoSNotSupported => write!(f, "QoS not supported"),
            Self::UseAnotherServer => write!(f, "Use another server"),
            Self::ServerMoved => write!(f, "Server moved"),
            Self::SharedSubscriptionsNotSupported => {
                write!(f, "Shared subscriptions not supported")
            }
            Self::ConnectionRateExceeded => write!(f, "Connection rate exceeded"),
            Self::SubscriptionIdentifiersNotSupported => {
                write!(f, "Subscription identifiers not supported")
            }
            Self::WildcardSubscriptionsNotSupported => {
                write!(f, "Wildcard subscriptions not supported")
            }
            Self::MessageTooLarge => write!(f, "Message too large for queue"),
            Self::FlowControlExceeded => write!(f, "Flow control exceeded"),
            Self::PacketIdExhausted => write!(f, "Packet ID exhausted"),
            Self::StringTooLong(len) => {
                write!(f, "String too long: {len} bytes exceeds maximum of 65535")
            }
            Self::Configuration(s) => write!(f, "Configuration error: {s}"),
        }
    }
}

impl MqttError {
    #[must_use]
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

#[cfg(feature = "std")]
impl From<std::io::Error> for MqttError {
    fn from(err: std::io::Error) -> Self {
        MqttError::Io(err.to_string())
    }
}

impl From<String> for MqttError {
    fn from(msg: String) -> Self {
        MqttError::MalformedPacket(msg)
    }
}

impl From<&str> for MqttError {
    fn from(msg: &str) -> Self {
        MqttError::MalformedPacket(crate::prelude::ToString::to_string(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(feature = "std")]
    #[test]
    fn test_error_from_io() {
        use std::io;
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
