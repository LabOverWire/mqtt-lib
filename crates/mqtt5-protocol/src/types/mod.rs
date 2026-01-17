mod connect;
mod message;
mod publish;
mod subscribe;

pub use crate::protocol::v5::reason_codes::ReasonCode;
pub use connect::{ConnectOptions, ConnectProperties, ConnectResult};
pub use message::{Message, MessageProperties, WillMessage, WillProperties};
pub use publish::{PublishOptions, PublishProperties, PublishResult};
pub use subscribe::{RetainHandling, SubscribeOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProtocolVersion {
    V311,
    #[default]
    V5,
}

impl ProtocolVersion {
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            ProtocolVersion::V311 => 4,
            ProtocolVersion::V5 => 5,
        }
    }
}

impl From<ProtocolVersion> for u8 {
    fn from(version: ProtocolVersion) -> Self {
        version.as_u8()
    }
}

impl TryFrom<u8> for ProtocolVersion {
    type Error = ();

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            4 => Ok(ProtocolVersion::V311),
            5 => Ok(ProtocolVersion::V5),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum QoS {
    AtMostOnce = 0,
    AtLeastOnce = 1,
    ExactlyOnce = 2,
}

impl From<u8> for QoS {
    fn from(value: u8) -> Self {
        match value {
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => QoS::AtMostOnce,
        }
    }
}

impl From<QoS> for u8 {
    fn from(qos: QoS) -> Self {
        qos as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qos_values() {
        assert_eq!(QoS::AtMostOnce as u8, 0);
        assert_eq!(QoS::AtLeastOnce as u8, 1);
        assert_eq!(QoS::ExactlyOnce as u8, 2);
    }

    #[test]
    fn test_qos_from_u8() {
        assert_eq!(QoS::from(0), QoS::AtMostOnce);
        assert_eq!(QoS::from(1), QoS::AtLeastOnce);
        assert_eq!(QoS::from(2), QoS::ExactlyOnce);

        assert_eq!(QoS::from(3), QoS::AtMostOnce);
        assert_eq!(QoS::from(255), QoS::AtMostOnce);
    }

    #[test]
    fn test_qos_into_u8() {
        assert_eq!(u8::from(QoS::AtMostOnce), 0);
        assert_eq!(u8::from(QoS::AtLeastOnce), 1);
        assert_eq!(u8::from(QoS::ExactlyOnce), 2);
    }
}
