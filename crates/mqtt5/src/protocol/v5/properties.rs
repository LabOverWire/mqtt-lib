use crate::encoding::{
    decode_binary, decode_string, decode_variable_int, encode_binary, encode_string,
    encode_variable_int,
};
use crate::error::{MqttError, Result};
use bytes::{Buf, BufMut, Bytes};
use std::collections::HashMap;

/// MQTT v5.0 Property Identifiers
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PropertyId {
    // Byte properties
    PayloadFormatIndicator = 0x01,
    RequestProblemInformation = 0x17,
    RequestResponseInformation = 0x19,
    MaximumQoS = 0x24,
    RetainAvailable = 0x25,
    WildcardSubscriptionAvailable = 0x28,
    SubscriptionIdentifierAvailable = 0x29,
    SharedSubscriptionAvailable = 0x2A,

    // Two Byte Integer properties
    ServerKeepAlive = 0x13,
    ReceiveMaximum = 0x21,
    TopicAliasMaximum = 0x22,
    TopicAlias = 0x23,

    // Four Byte Integer properties
    MessageExpiryInterval = 0x02,
    SessionExpiryInterval = 0x11,
    WillDelayInterval = 0x18,
    MaximumPacketSize = 0x27,

    // Variable Byte Integer properties
    SubscriptionIdentifier = 0x0B,

    // UTF-8 Encoded String properties
    ContentType = 0x03,
    ResponseTopic = 0x08,
    AssignedClientIdentifier = 0x12,
    AuthenticationMethod = 0x15,
    ResponseInformation = 0x1A,
    ServerReference = 0x1C,
    ReasonString = 0x1F,

    // Binary Data properties
    CorrelationData = 0x09,
    AuthenticationData = 0x16,

    // UTF-8 String Pair properties
    UserProperty = 0x26,
}

impl PropertyId {
    /// Converts a u8 to `PropertyId`
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

    /// Checks if this property can appear multiple times in a packet
    #[must_use]
    pub fn allows_multiple(&self) -> bool {
        matches!(self, Self::UserProperty | Self::SubscriptionIdentifier)
    }

    /// Gets the expected value type for this property
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

/// Property value types
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

/// Property value storage
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyValue {
    Byte(u8),
    TwoByteInteger(u16),
    FourByteInteger(u32),
    VariableByteInteger(u32),
    BinaryData(Bytes),
    Utf8String(String),
    Utf8StringPair(String, String),
}

impl PropertyValue {
    /// Gets the value type
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

    /// Validates that this value matches the expected type for a property
    #[must_use]
    pub fn matches_type(&self, expected: PropertyValueType) -> bool {
        self.value_type() == expected
    }
}

/// Container for MQTT v5.0 properties
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Properties {
    properties: HashMap<PropertyId, Vec<PropertyValue>>,
}

impl Properties {
    /// Creates a new empty properties container
    #[must_use]
    pub fn new() -> Self {
        Self {
            properties: HashMap::new(),
        }
    }

    /// Adds a property value
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The value type doesn't match the property's expected type
    /// - The property doesn't allow multiple values and already exists
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn add(&mut self, id: PropertyId, value: PropertyValue) -> Result<()> {
        // Validate value type
        if !value.matches_type(id.value_type()) {
            return Err(MqttError::ProtocolError(format!(
                "Property {:?} expects type {:?}, got {:?}",
                id,
                id.value_type(),
                value.value_type()
            )));
        }

        // Check if property allows multiple values
        if !id.allows_multiple() && self.properties.contains_key(&id) {
            return Err(MqttError::DuplicatePropertyId(id as u8));
        }

        self.properties.entry(id).or_default().push(value);
        Ok(())
    }

    /// Gets a single property value
    #[must_use]
    pub fn get(&self, id: PropertyId) -> Option<&PropertyValue> {
        self.properties.get(&id).and_then(|v| v.first())
    }

    /// Gets all values for a property (for properties that allow multiple values)
    #[must_use]
    pub fn get_all(&self, id: PropertyId) -> Option<&[PropertyValue]> {
        self.properties.get(&id).map(std::vec::Vec::as_slice)
    }

    /// Checks if a property is present
    #[must_use]
    pub fn contains(&self, id: PropertyId) -> bool {
        self.properties.contains_key(&id)
    }

    /// Returns the number of properties (counting multi-value properties as one)
    #[must_use]
    pub fn len(&self) -> usize {
        self.properties.len()
    }

    /// Checks if there are no properties
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    /// Iterates over all properties and their values
    pub fn iter(&self) -> impl Iterator<Item = (PropertyId, &PropertyValue)> + '_ {
        self.properties
            .iter()
            .flat_map(|(id, values)| values.iter().map(move |value| (*id, value)))
    }

    /// Encodes all properties to the buffer
    ///
    /// # Errors
    ///
    /// Returns an error if encoding fails
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn encode<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // First calculate the total properties length
        let mut props_buf = Vec::new();
        self.encode_properties(&mut props_buf)?;

        // Write properties length as variable byte integer
        encode_variable_int(
            buf,
            props_buf
                .len()
                .try_into()
                .map_err(|_| MqttError::PacketTooLarge {
                    size: props_buf.len(),
                    max: u32::MAX as usize,
                })?,
        )?;

        // Write properties data
        buf.put_slice(&props_buf);
        Ok(())
    }

    /// Encodes properties without the length prefix
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    fn encode_properties<B: BufMut>(&self, buf: &mut B) -> Result<()> {
        // Sort properties by ID for consistent encoding
        let mut sorted_props: Vec<_> = self.properties.iter().collect();
        sorted_props.sort_by_key(|(id, _)| **id as u8);

        for (id, values) in sorted_props {
            for value in values {
                // Write property identifier as variable byte integer
                encode_variable_int(buf, u32::from(*id as u8))?;

                // Write property value
                match value {
                    PropertyValue::Byte(v) => buf.put_u8(*v),
                    PropertyValue::TwoByteInteger(v) => buf.put_u16(*v),
                    PropertyValue::FourByteInteger(v) => buf.put_u32(*v),
                    PropertyValue::VariableByteInteger(v) => encode_variable_int(buf, *v)?,
                    PropertyValue::BinaryData(v) => encode_binary(buf, v)?,
                    PropertyValue::Utf8String(v) => encode_string(buf, v)?,
                    PropertyValue::Utf8StringPair(k, v) => {
                        encode_string(buf, k)?;
                        encode_string(buf, v)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Decodes properties from the buffer
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - Decoding fails
    /// - Invalid property ID
    /// - Type mismatch
    /// - Duplicate property that doesn't allow multiples
    ///
    /// # Errors
    ///
    /// Returns an error if the operation fails
    pub fn decode<B: Buf>(buf: &mut B) -> Result<Self> {
        // Read properties length
        let props_len = decode_variable_int(buf)? as usize;

        // Create a sub-buffer for properties
        if buf.remaining() < props_len {
            return Err(MqttError::MalformedPacket(format!(
                "Insufficient data for properties: expected {}, got {}",
                props_len,
                buf.remaining()
            )));
        }

        let mut props_buf = buf.copy_to_bytes(props_len);
        let mut properties = Self::new();

        while props_buf.has_remaining() {
            // Read property identifier
            let id_val = decode_variable_int(&mut props_buf)?;
            let id_byte = u8::try_from(id_val).map_err(|_| MqttError::InvalidPropertyId(255))?;

            let id = PropertyId::from_u8(id_byte).ok_or(MqttError::InvalidPropertyId(id_byte))?;

            // Read property value based on type
            let value = match id.value_type() {
                PropertyValueType::Byte => {
                    if !props_buf.has_remaining() {
                        return Err(MqttError::MalformedPacket(
                            "Insufficient data for byte property".to_string(),
                        ));
                    }
                    PropertyValue::Byte(props_buf.get_u8())
                }
                PropertyValueType::TwoByteInteger => {
                    if props_buf.remaining() < 2 {
                        return Err(MqttError::MalformedPacket(
                            "Insufficient data for two-byte integer property".to_string(),
                        ));
                    }
                    PropertyValue::TwoByteInteger(props_buf.get_u16())
                }
                PropertyValueType::FourByteInteger => {
                    if props_buf.remaining() < 4 {
                        return Err(MqttError::MalformedPacket(
                            "Insufficient data for four-byte integer property".to_string(),
                        ));
                    }
                    PropertyValue::FourByteInteger(props_buf.get_u32())
                }
                PropertyValueType::VariableByteInteger => {
                    PropertyValue::VariableByteInteger(decode_variable_int(&mut props_buf)?)
                }
                PropertyValueType::BinaryData => {
                    PropertyValue::BinaryData(decode_binary(&mut props_buf)?)
                }
                PropertyValueType::Utf8String => {
                    PropertyValue::Utf8String(decode_string(&mut props_buf)?)
                }
                PropertyValueType::Utf8StringPair => {
                    let key = decode_string(&mut props_buf)?;
                    let value = decode_string(&mut props_buf)?;
                    PropertyValue::Utf8StringPair(key, value)
                }
            };

            properties.add(id, value)?;
        }

        Ok(properties)
    }

    /// Calculates the encoded length of all properties (including length prefix)
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        let props_len = self.properties_encoded_len();
        crate::encoding::variable_int_len(props_len.try_into().unwrap_or(u32::MAX)) + props_len
    }

    /// Calculates the encoded length of properties without length prefix
    #[must_use]
    fn properties_encoded_len(&self) -> usize {
        let mut len = 0;

        for (id, values) in &self.properties {
            for value in values {
                // Property ID length
                len += crate::encoding::variable_int_len(u32::from(*id as u8));

                // Property value length
                len += match value {
                    PropertyValue::Byte(_) => 1,
                    PropertyValue::TwoByteInteger(_) => 2,
                    PropertyValue::FourByteInteger(_) => 4,
                    PropertyValue::VariableByteInteger(v) => crate::encoding::variable_int_len(*v),
                    PropertyValue::BinaryData(v) => crate::encoding::binary_len(v),
                    PropertyValue::Utf8String(v) => crate::encoding::string_len(v),
                    PropertyValue::Utf8StringPair(k, v) => {
                        crate::encoding::string_len(k) + crate::encoding::string_len(v)
                    }
                };
            }
        }

        len
    }

    // Type-safe property setters that cannot fail

    /// Sets the payload format indicator (0 = unspecified bytes, 1 = UTF-8)
    pub fn set_payload_format_indicator(&mut self, is_utf8: bool) {
        self.properties
            .entry(PropertyId::PayloadFormatIndicator)
            .or_default()
            .push(PropertyValue::Byte(u8::from(is_utf8)));
    }

    /// Sets the message expiry interval in seconds
    pub fn set_message_expiry_interval(&mut self, seconds: u32) {
        self.properties
            .entry(PropertyId::MessageExpiryInterval)
            .or_default()
            .push(PropertyValue::FourByteInteger(seconds));
    }

    /// Sets the topic alias
    pub fn set_topic_alias(&mut self, alias: u16) {
        self.properties
            .entry(PropertyId::TopicAlias)
            .or_default()
            .push(PropertyValue::TwoByteInteger(alias));
    }

    /// Sets the response topic
    pub fn set_response_topic(&mut self, topic: String) {
        self.properties
            .entry(PropertyId::ResponseTopic)
            .or_default()
            .push(PropertyValue::Utf8String(topic));
    }

    /// Sets the correlation data
    pub fn set_correlation_data(&mut self, data: Bytes) {
        self.properties
            .entry(PropertyId::CorrelationData)
            .or_default()
            .push(PropertyValue::BinaryData(data));
    }

    /// Adds a user property (can be called multiple times)
    pub fn add_user_property(&mut self, key: String, value: String) {
        self.properties
            .entry(PropertyId::UserProperty)
            .or_default()
            .push(PropertyValue::Utf8StringPair(key, value));
    }

    /// Sets the subscription identifier
    pub fn set_subscription_identifier(&mut self, id: u32) {
        self.properties
            .entry(PropertyId::SubscriptionIdentifier)
            .or_default()
            .push(PropertyValue::VariableByteInteger(id));
    }

    /// Sets the session expiry interval
    pub fn set_session_expiry_interval(&mut self, seconds: u32) {
        self.properties
            .entry(PropertyId::SessionExpiryInterval)
            .or_default()
            .push(PropertyValue::FourByteInteger(seconds));
    }

    /// Gets the session expiry interval
    pub fn get_session_expiry_interval(&self) -> Option<u32> {
        self.properties
            .get(&PropertyId::SessionExpiryInterval)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::FourByteInteger(v) = value {
                    Some(*v)
                } else {
                    None
                }
            })
    }

    /// Sets the assigned client identifier
    pub fn set_assigned_client_identifier(&mut self, id: String) {
        self.properties
            .entry(PropertyId::AssignedClientIdentifier)
            .or_default()
            .push(PropertyValue::Utf8String(id));
    }

    /// Sets the server keep alive
    pub fn set_server_keep_alive(&mut self, seconds: u16) {
        self.properties
            .entry(PropertyId::ServerKeepAlive)
            .or_default()
            .push(PropertyValue::TwoByteInteger(seconds));
    }

    /// Sets the authentication method
    pub fn set_authentication_method(&mut self, method: String) {
        self.properties
            .entry(PropertyId::AuthenticationMethod)
            .or_default()
            .push(PropertyValue::Utf8String(method));
    }

    /// Sets the authentication data
    pub fn set_authentication_data(&mut self, data: Bytes) {
        self.properties
            .entry(PropertyId::AuthenticationData)
            .or_default()
            .push(PropertyValue::BinaryData(data));
    }

    /// Sets request problem information
    pub fn set_request_problem_information(&mut self, request: bool) {
        self.properties
            .entry(PropertyId::RequestProblemInformation)
            .or_default()
            .push(PropertyValue::Byte(u8::from(request)));
    }

    /// Sets the will delay interval
    pub fn set_will_delay_interval(&mut self, seconds: u32) {
        self.properties
            .entry(PropertyId::WillDelayInterval)
            .or_default()
            .push(PropertyValue::FourByteInteger(seconds));
    }

    /// Sets request response information
    pub fn set_request_response_information(&mut self, request: bool) {
        self.properties
            .entry(PropertyId::RequestResponseInformation)
            .or_default()
            .push(PropertyValue::Byte(u8::from(request)));
    }

    /// Sets the response information
    pub fn set_response_information(&mut self, info: String) {
        self.properties
            .entry(PropertyId::ResponseInformation)
            .or_default()
            .push(PropertyValue::Utf8String(info));
    }

    /// Sets the server reference
    pub fn set_server_reference(&mut self, reference: String) {
        self.properties
            .entry(PropertyId::ServerReference)
            .or_default()
            .push(PropertyValue::Utf8String(reference));
    }

    /// Sets the reason string
    pub fn set_reason_string(&mut self, reason: String) {
        self.properties
            .entry(PropertyId::ReasonString)
            .or_default()
            .push(PropertyValue::Utf8String(reason));
    }

    /// Sets the receive maximum
    pub fn set_receive_maximum(&mut self, max: u16) {
        self.properties
            .entry(PropertyId::ReceiveMaximum)
            .or_default()
            .push(PropertyValue::TwoByteInteger(max));
    }

    /// Sets the topic alias maximum
    pub fn set_topic_alias_maximum(&mut self, max: u16) {
        self.properties
            .entry(PropertyId::TopicAliasMaximum)
            .or_default()
            .push(PropertyValue::TwoByteInteger(max));
    }

    /// Sets the maximum `QoS`
    pub fn set_maximum_qos(&mut self, qos: u8) {
        self.properties
            .entry(PropertyId::MaximumQoS)
            .or_default()
            .push(PropertyValue::Byte(qos));
    }

    /// Sets retain available
    pub fn set_retain_available(&mut self, available: bool) {
        self.properties
            .entry(PropertyId::RetainAvailable)
            .or_default()
            .push(PropertyValue::Byte(u8::from(available)));
    }

    /// Sets the maximum packet size
    pub fn set_maximum_packet_size(&mut self, size: u32) {
        self.properties
            .entry(PropertyId::MaximumPacketSize)
            .or_default()
            .push(PropertyValue::FourByteInteger(size));
    }

    /// Sets wildcard subscription available
    pub fn set_wildcard_subscription_available(&mut self, available: bool) {
        self.properties
            .entry(PropertyId::WildcardSubscriptionAvailable)
            .or_default()
            .push(PropertyValue::Byte(u8::from(available)));
    }

    /// Sets subscription identifier available
    pub fn set_subscription_identifier_available(&mut self, available: bool) {
        self.properties
            .entry(PropertyId::SubscriptionIdentifierAvailable)
            .or_default()
            .push(PropertyValue::Byte(u8::from(available)));
    }

    /// Sets shared subscription available
    pub fn set_shared_subscription_available(&mut self, available: bool) {
        self.properties
            .entry(PropertyId::SharedSubscriptionAvailable)
            .or_default()
            .push(PropertyValue::Byte(u8::from(available)));
    }

    /// Gets the maximum QoS
    pub fn get_maximum_qos(&self) -> Option<u8> {
        self.properties
            .get(&PropertyId::MaximumQoS)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::Byte(v) = value {
                    Some(*v)
                } else {
                    None
                }
            })
    }

    /// Sets the content type
    pub fn set_content_type(&mut self, content_type: String) {
        self.properties
            .entry(PropertyId::ContentType)
            .or_default()
            .push(PropertyValue::Utf8String(content_type));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::BytesMut;

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

        // Add single-value properties
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

        // Add multi-value property
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

        assert_eq!(props.len(), 4); // 4 unique property IDs
    }

    #[test]
    fn test_properties_add_type_mismatch() {
        let mut props = Properties::new();

        // Try to add wrong type
        let result = props.add(
            PropertyId::PayloadFormatIndicator,
            PropertyValue::FourByteInteger(100),
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_properties_add_duplicate_single_value() {
        let mut props = Properties::new();

        // Add first time - should succeed
        props
            .add(PropertyId::PayloadFormatIndicator, PropertyValue::Byte(0))
            .unwrap();

        // Add second time - should fail
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

        // Add multiple user properties
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
        assert_eq!(buf[0], 0); // Empty properties = 0 length

        let decoded = Properties::decode(&mut buf).unwrap();
        assert!(decoded.is_empty());
    }

    #[test]
    fn test_properties_encode_decode_single_values() {
        let mut props = Properties::new();

        // Add various property types
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

        // Verify each property
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

        // Add multiple user properties
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

        // Add multiple subscription identifiers
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
        let mut buf = BytesMut::new();
        buf.put_u8(1); // Properties length
        buf.put_u8(0xFF); // Invalid property ID

        let result = Properties::decode(&mut buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_properties_decode_insufficient_data() {
        let mut buf = BytesMut::new();
        buf.put_u8(10); // Claims 10 bytes but buffer is empty

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
        // Test that all property IDs in from_u8 have matching value types
        for id in 0u8..=0x2A {
            if let Some(prop_id) = PropertyId::from_u8(id) {
                // Just verify it has a value type (no panic)
                let _ = prop_id.value_type();
            }
        }
    }
}
