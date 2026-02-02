mod accessors;
mod codec;

use crate::error::{MqttError, Result};
use crate::prelude::{format, HashMap, String, Vec};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropertyId {
    PayloadFormatIndicator = 0x01,
    RequestProblemInformation = 0x17,
    RequestResponseInformation = 0x19,
    MaximumQoS = 0x24,
    RetainAvailable = 0x25,
    WildcardSubscriptionAvailable = 0x28,
    SubscriptionIdentifierAvailable = 0x29,
    SharedSubscriptionAvailable = 0x2A,

    ServerKeepAlive = 0x13,
    ReceiveMaximum = 0x21,
    TopicAliasMaximum = 0x22,
    TopicAlias = 0x23,

    MessageExpiryInterval = 0x02,
    SessionExpiryInterval = 0x11,
    WillDelayInterval = 0x18,
    MaximumPacketSize = 0x27,

    SubscriptionIdentifier = 0x0B,

    ContentType = 0x03,
    ResponseTopic = 0x08,
    AssignedClientIdentifier = 0x12,
    AuthenticationMethod = 0x15,
    ResponseInformation = 0x1A,
    ServerReference = 0x1C,
    ReasonString = 0x1F,

    CorrelationData = 0x09,
    AuthenticationData = 0x16,

    UserProperty = 0x26,
}

impl PropertyId {
    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::PayloadFormatIndicator),
            0x02 => Some(Self::MessageExpiryInterval),
            0x03 => Some(Self::ContentType),
            0x08 => Some(Self::ResponseTopic),
            0x09 => Some(Self::CorrelationData),
            0x0B => Some(Self::SubscriptionIdentifier),
            0x11 => Some(Self::SessionExpiryInterval),
            0x12 => Some(Self::AssignedClientIdentifier),
            0x13 => Some(Self::ServerKeepAlive),
            0x15 => Some(Self::AuthenticationMethod),
            0x16 => Some(Self::AuthenticationData),
            0x17 => Some(Self::RequestProblemInformation),
            0x18 => Some(Self::WillDelayInterval),
            0x19 => Some(Self::RequestResponseInformation),
            0x1A => Some(Self::ResponseInformation),
            0x1C => Some(Self::ServerReference),
            0x1F => Some(Self::ReasonString),
            0x21 => Some(Self::ReceiveMaximum),
            0x22 => Some(Self::TopicAliasMaximum),
            0x23 => Some(Self::TopicAlias),
            0x24 => Some(Self::MaximumQoS),
            0x25 => Some(Self::RetainAvailable),
            0x26 => Some(Self::UserProperty),
            0x27 => Some(Self::MaximumPacketSize),
            0x28 => Some(Self::WildcardSubscriptionAvailable),
            0x29 => Some(Self::SubscriptionIdentifierAvailable),
            0x2A => Some(Self::SharedSubscriptionAvailable),
            _ => None,
        }
    }

    #[must_use]
    pub fn allows_multiple(&self) -> bool {
        matches!(self, Self::UserProperty | Self::SubscriptionIdentifier)
    }

    #[must_use]
    pub fn value_type(&self) -> PropertyValueType {
        match self {
            Self::PayloadFormatIndicator
            | Self::RequestProblemInformation
            | Self::RequestResponseInformation
            | Self::MaximumQoS
            | Self::RetainAvailable
            | Self::WildcardSubscriptionAvailable
            | Self::SubscriptionIdentifierAvailable
            | Self::SharedSubscriptionAvailable => PropertyValueType::Byte,

            Self::ServerKeepAlive
            | Self::ReceiveMaximum
            | Self::TopicAliasMaximum
            | Self::TopicAlias => PropertyValueType::TwoByteInteger,

            Self::MessageExpiryInterval
            | Self::SessionExpiryInterval
            | Self::WillDelayInterval
            | Self::MaximumPacketSize => PropertyValueType::FourByteInteger,

            Self::SubscriptionIdentifier => PropertyValueType::VariableByteInteger,

            Self::ContentType
            | Self::ResponseTopic
            | Self::AssignedClientIdentifier
            | Self::AuthenticationMethod
            | Self::ResponseInformation
            | Self::ServerReference
            | Self::ReasonString => PropertyValueType::Utf8String,

            Self::CorrelationData | Self::AuthenticationData => PropertyValueType::BinaryData,

            Self::UserProperty => PropertyValueType::Utf8StringPair,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyValueType {
    Byte,
    TwoByteInteger,
    FourByteInteger,
    VariableByteInteger,
    BinaryData,
    Utf8String,
    Utf8StringPair,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyValue {
    Byte(u8),
    TwoByteInteger(u16),
    FourByteInteger(u32),
    VariableByteInteger(u32),
    BinaryData(bytes::Bytes),
    Utf8String(String),
    Utf8StringPair(String, String),
}

impl PropertyValue {
    #[must_use]
    pub fn value_type(&self) -> PropertyValueType {
        match self {
            Self::Byte(_) => PropertyValueType::Byte,
            Self::TwoByteInteger(_) => PropertyValueType::TwoByteInteger,
            Self::FourByteInteger(_) => PropertyValueType::FourByteInteger,
            Self::VariableByteInteger(_) => PropertyValueType::VariableByteInteger,
            Self::BinaryData(_) => PropertyValueType::BinaryData,
            Self::Utf8String(_) => PropertyValueType::Utf8String,
            Self::Utf8StringPair(_, _) => PropertyValueType::Utf8StringPair,
        }
    }

    #[must_use]
    pub fn matches_type(&self, expected: PropertyValueType) -> bool {
        self.value_type() == expected
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Properties {
    pub(crate) properties: HashMap<PropertyId, Vec<PropertyValue>>,
}

impl Properties {
    #[must_use]
    pub fn new() -> Self {
        Self {
            properties: HashMap::new(),
        }
    }

    /// # Errors
    /// Returns error if value type doesn't match property's expected type
    /// or if property doesn't allow multiple values and already exists.
    pub fn add(&mut self, id: PropertyId, value: PropertyValue) -> Result<()> {
        if !value.matches_type(id.value_type()) {
            return Err(MqttError::ProtocolError(format!(
                "Property {:?} expects type {:?}, got {:?}",
                id,
                id.value_type(),
                value.value_type()
            )));
        }

        if !id.allows_multiple() && self.properties.contains_key(&id) {
            return Err(MqttError::DuplicatePropertyId(id as u8));
        }

        self.properties.entry(id).or_default().push(value);
        Ok(())
    }

    #[must_use]
    pub fn get(&self, id: PropertyId) -> Option<&PropertyValue> {
        self.properties.get(&id).and_then(|v| v.first())
    }

    #[must_use]
    pub fn get_all(&self, id: PropertyId) -> Option<&[PropertyValue]> {
        self.properties.get(&id).map(Vec::as_slice)
    }

    #[must_use]
    pub fn contains(&self, id: PropertyId) -> bool {
        self.properties.contains_key(&id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.properties.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (PropertyId, &PropertyValue)> + '_ {
        self.properties
            .iter()
            .flat_map(|(id, values)| values.iter().map(move |value| (*id, value)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prelude::ToString;
    use bytes::{Bytes, BytesMut};

    #[test]
    fn test_property_id_from_u8() {
        assert_eq!(
            PropertyId::from_u8(0x01),
            Some(PropertyId::PayloadFormatIndicator)
        );
        assert_eq!(PropertyId::from_u8(0x26), Some(PropertyId::UserProperty));
        assert_eq!(
            PropertyId::from_u8(0x2A),
            Some(PropertyId::SharedSubscriptionAvailable)
        );
        assert_eq!(PropertyId::from_u8(0xFF), None);
        assert_eq!(PropertyId::from_u8(0x00), None);
    }

    #[test]
    fn test_property_allows_multiple() {
        assert!(PropertyId::UserProperty.allows_multiple());
        assert!(PropertyId::SubscriptionIdentifier.allows_multiple());
        assert!(!PropertyId::PayloadFormatIndicator.allows_multiple());
        assert!(!PropertyId::SessionExpiryInterval.allows_multiple());
    }

    #[test]
    fn test_property_value_type() {
        assert_eq!(
            PropertyId::PayloadFormatIndicator.value_type(),
            PropertyValueType::Byte
        );
        assert_eq!(
            PropertyId::TopicAlias.value_type(),
            PropertyValueType::TwoByteInteger
        );
        assert_eq!(
            PropertyId::SessionExpiryInterval.value_type(),
            PropertyValueType::FourByteInteger
        );
        assert_eq!(
            PropertyId::SubscriptionIdentifier.value_type(),
            PropertyValueType::VariableByteInteger
        );
        assert_eq!(
            PropertyId::ContentType.value_type(),
            PropertyValueType::Utf8String
        );
        assert_eq!(
            PropertyId::CorrelationData.value_type(),
            PropertyValueType::BinaryData
        );
        assert_eq!(
            PropertyId::UserProperty.value_type(),
            PropertyValueType::Utf8StringPair
        );
    }

    #[test]
    fn test_property_value_matches_type() {
        let byte_val = PropertyValue::Byte(1);
        assert!(byte_val.matches_type(PropertyValueType::Byte));
        assert!(!byte_val.matches_type(PropertyValueType::TwoByteInteger));

        let string_val = PropertyValue::Utf8String("test".to_string());
        assert!(string_val.matches_type(PropertyValueType::Utf8String));
        assert!(!string_val.matches_type(PropertyValueType::BinaryData));
    }

    #[test]
    fn test_properties_add_valid() {
        let mut props = Properties::new();

        props
            .add(PropertyId::PayloadFormatIndicator, PropertyValue::Byte(1))
            .unwrap();
        props
            .add(
                PropertyId::SessionExpiryInterval,
                PropertyValue::FourByteInteger(3600),
            )
            .unwrap();
        props
            .add(
                PropertyId::ContentType,
                PropertyValue::Utf8String("text/plain".to_string()),
            )
            .unwrap();

        props
            .add(
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair("key1".to_string(), "value1".to_string()),
            )
            .unwrap();
        props
            .add(
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair("key2".to_string(), "value2".to_string()),
            )
            .unwrap();

        assert_eq!(props.len(), 4);
    }

    #[test]
    fn test_properties_add_type_mismatch() {
        let mut props = Properties::new();

        let result = props.add(
            PropertyId::PayloadFormatIndicator,
            PropertyValue::FourByteInteger(100),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_properties_add_duplicate_single_value() {
        let mut props = Properties::new();

        props
            .add(PropertyId::PayloadFormatIndicator, PropertyValue::Byte(0))
            .unwrap();

        let result = props.add(PropertyId::PayloadFormatIndicator, PropertyValue::Byte(1));
        assert!(result.is_err());
    }

    #[test]
    fn test_properties_get() {
        let mut props = Properties::new();
        props
            .add(
                PropertyId::ContentType,
                PropertyValue::Utf8String("text/html".to_string()),
            )
            .unwrap();

        let value = props.get(PropertyId::ContentType).unwrap();
        match value {
            PropertyValue::Utf8String(s) => assert_eq!(s, "text/html"),
            _ => panic!("Wrong value type"),
        }

        assert!(props.get(PropertyId::ResponseTopic).is_none());
    }

    #[test]
    fn test_properties_get_all() {
        let mut props = Properties::new();

        props
            .add(
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair("k1".to_string(), "v1".to_string()),
            )
            .unwrap();
        props
            .add(
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair("k2".to_string(), "v2".to_string()),
            )
            .unwrap();

        let values = props.get_all(PropertyId::UserProperty).unwrap();
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn test_properties_encode_decode_empty() {
        let props = Properties::new();
        let mut buf = BytesMut::new();

        props.encode(&mut buf).unwrap();
        assert_eq!(buf[0], 0);

        let decoded = Properties::decode(&mut buf).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_properties_encode_decode_single_values() {
        let mut props = Properties::new();

        props
            .add(PropertyId::PayloadFormatIndicator, PropertyValue::Byte(1))
            .unwrap();
        props
            .add(PropertyId::TopicAlias, PropertyValue::TwoByteInteger(100))
            .unwrap();
        props
            .add(
                PropertyId::SessionExpiryInterval,
                PropertyValue::FourByteInteger(3600),
            )
            .unwrap();
        props
            .add(
                PropertyId::SubscriptionIdentifier,
                PropertyValue::VariableByteInteger(123),
            )
            .unwrap();
        props
            .add(
                PropertyId::ContentType,
                PropertyValue::Utf8String("text/plain".to_string()),
            )
            .unwrap();
        props
            .add(
                PropertyId::CorrelationData,
                PropertyValue::BinaryData(Bytes::from(vec![1, 2, 3, 4])),
            )
            .unwrap();
        props
            .add(
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair("key".to_string(), "value".to_string()),
            )
            .unwrap();

        let mut buf = BytesMut::new();
        props.encode(&mut buf).unwrap();

        let decoded = Properties::decode(&mut buf).unwrap();
        assert_eq!(decoded.len(), props.len());

        match decoded.get(PropertyId::PayloadFormatIndicator).unwrap() {
            PropertyValue::Byte(v) => assert_eq!(*v, 1),
            _ => panic!("Wrong type"),
        }

        match decoded.get(PropertyId::TopicAlias).unwrap() {
            PropertyValue::TwoByteInteger(v) => assert_eq!(*v, 100),
            _ => panic!("Wrong type"),
        }

        match decoded.get(PropertyId::ContentType).unwrap() {
            PropertyValue::Utf8String(v) => assert_eq!(v, "text/plain"),
            _ => panic!("Wrong type"),
        }
    }

    #[test]
    fn test_properties_encode_decode_multiple_values() {
        let mut props = Properties::new();

        props
            .add(
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair("env".to_string(), "prod".to_string()),
            )
            .unwrap();
        props
            .add(
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair("version".to_string(), "1.0".to_string()),
            )
            .unwrap();

        props
            .add(
                PropertyId::SubscriptionIdentifier,
                PropertyValue::VariableByteInteger(10),
            )
            .unwrap();
        props
            .add(
                PropertyId::SubscriptionIdentifier,
                PropertyValue::VariableByteInteger(20),
            )
            .unwrap();

        let mut buf = BytesMut::new();
        props.encode(&mut buf).unwrap();

        let decoded = Properties::decode(&mut buf).unwrap();

        let user_props = decoded.get_all(PropertyId::UserProperty).unwrap();
        assert_eq!(user_props.len(), 2);

        let sub_ids = decoded.get_all(PropertyId::SubscriptionIdentifier).unwrap();
        assert_eq!(sub_ids.len(), 2);
    }

    #[test]
    fn test_properties_decode_invalid_property_id() {
        use bytes::BufMut;
        let mut buf = BytesMut::new();
        buf.put_u8(1);
        buf.put_u8(0xFF);

        let result = Properties::decode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_properties_decode_insufficient_data() {
        use bytes::BufMut;
        let mut buf = BytesMut::new();
        buf.put_u8(10);

        let result = Properties::decode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_properties_encoded_len() {
        let mut props = Properties::new();
        props
            .add(PropertyId::PayloadFormatIndicator, PropertyValue::Byte(1))
            .unwrap();
        props
            .add(
                PropertyId::ContentType,
                PropertyValue::Utf8String("test".to_string()),
            )
            .unwrap();

        let mut buf = BytesMut::new();
        props.encode(&mut buf).unwrap();

        assert_eq!(props.encoded_len(), buf.len());
    }

    #[test]
    fn test_all_property_ids_have_correct_types() {
        for id in 0u8..=0x2A {
            if let Some(prop_id) = PropertyId::from_u8(id) {
                let _ = prop_id.value_type();
            }
        }
    }

    #[test]
    fn test_remove_user_property_by_key() {
        let mut props = Properties::new();
        props.add_user_property("x-mqtt-sender".to_string(), "attacker".to_string());
        props.add_user_property("other-key".to_string(), "value".to_string());
        props.add_user_property("x-mqtt-sender".to_string(), "double".to_string());

        let all = props.get_all(PropertyId::UserProperty).unwrap();
        assert_eq!(all.len(), 3);

        props.remove_user_property_by_key("x-mqtt-sender");

        let remaining = props.get_all(PropertyId::UserProperty).unwrap();
        assert_eq!(remaining.len(), 1);
        match &remaining[0] {
            PropertyValue::Utf8StringPair(k, v) => {
                assert_eq!(k, "other-key");
                assert_eq!(v, "value");
            }
            _ => panic!("unexpected variant"),
        }
    }

    #[test]
    fn test_remove_user_property_by_key_removes_entry_when_empty() {
        let mut props = Properties::new();
        props.add_user_property("x-mqtt-sender".to_string(), "only".to_string());

        assert!(props.contains(PropertyId::UserProperty));

        props.remove_user_property_by_key("x-mqtt-sender");

        assert!(!props.contains(PropertyId::UserProperty));
    }

    #[test]
    fn test_remove_user_property_by_key_noop_when_absent() {
        let mut props = Properties::new();
        props.add_user_property("other".to_string(), "val".to_string());

        props.remove_user_property_by_key("x-mqtt-sender");

        let remaining = props.get_all(PropertyId::UserProperty).unwrap();
        assert_eq!(remaining.len(), 1);
    }
}
