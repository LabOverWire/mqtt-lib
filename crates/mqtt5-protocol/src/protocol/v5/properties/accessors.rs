use super::{Properties, PropertyId, PropertyValue};
use crate::prelude::String;
use bytes::Bytes;

impl Properties {
    pub fn set_payload_format_indicator(&mut self, is_utf8: bool) {
        self.properties
            .entry(PropertyId::PayloadFormatIndicator)
            .or_default()
            .push(PropertyValue::Byte(u8::from(is_utf8)));
    }

    pub fn set_message_expiry_interval(&mut self, seconds: u32) {
        self.properties
            .entry(PropertyId::MessageExpiryInterval)
            .or_default()
            .push(PropertyValue::FourByteInteger(seconds));
    }

    #[must_use]
    pub fn get_message_expiry_interval(&self) -> Option<u32> {
        self.properties
            .get(&PropertyId::MessageExpiryInterval)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::FourByteInteger(v) = value {
                    Some(*v)
                } else {
                    None
                }
            })
    }

    pub fn set_topic_alias(&mut self, alias: u16) {
        self.properties
            .entry(PropertyId::TopicAlias)
            .or_default()
            .push(PropertyValue::TwoByteInteger(alias));
    }

    #[must_use]
    pub fn get_topic_alias(&self) -> Option<u16> {
        self.properties
            .get(&PropertyId::TopicAlias)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::TwoByteInteger(v) = value {
                    Some(*v)
                } else {
                    None
                }
            })
    }

    pub fn set_response_topic(&mut self, topic: String) {
        self.properties
            .entry(PropertyId::ResponseTopic)
            .or_default()
            .push(PropertyValue::Utf8String(topic));
    }

    pub fn set_correlation_data(&mut self, data: Bytes) {
        self.properties
            .entry(PropertyId::CorrelationData)
            .or_default()
            .push(PropertyValue::BinaryData(data));
    }

    pub fn add_user_property(&mut self, key: String, value: String) {
        self.properties
            .entry(PropertyId::UserProperty)
            .or_default()
            .push(PropertyValue::Utf8StringPair(key, value));
    }

    pub fn remove_user_property_by_key(&mut self, key: &str) {
        if let Some(values) = self.properties.get_mut(&PropertyId::UserProperty) {
            values.retain(|v| {
                if let PropertyValue::Utf8StringPair(k, _) = v {
                    k != key
                } else {
                    true
                }
            });
            if values.is_empty() {
                self.properties.remove(&PropertyId::UserProperty);
            }
        }
    }

    pub fn inject_sender(&mut self, user_id: Option<&str>) {
        self.remove_user_property_by_key("x-mqtt-sender");
        if let Some(uid) = user_id {
            self.add_user_property("x-mqtt-sender".into(), uid.into());
        }
    }

    pub fn inject_client_id(&mut self, client_id: Option<&str>) {
        self.remove_user_property_by_key("x-mqtt-client-id");
        if let Some(cid) = client_id {
            self.add_user_property("x-mqtt-client-id".into(), cid.into());
        }
    }

    #[must_use]
    pub fn get_user_property_value(&self, key: &str) -> Option<&str> {
        self.properties
            .get(&PropertyId::UserProperty)?
            .iter()
            .find_map(|v| {
                if let PropertyValue::Utf8StringPair(k, val) = v {
                    if k == key {
                        return Some(val.as_str());
                    }
                }
                None
            })
    }

    pub fn set_subscription_identifier(&mut self, id: u32) {
        self.properties
            .entry(PropertyId::SubscriptionIdentifier)
            .or_default()
            .push(PropertyValue::VariableByteInteger(id));
    }

    #[must_use]
    pub fn get_subscription_identifier(&self) -> Option<u32> {
        self.properties
            .get(&PropertyId::SubscriptionIdentifier)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::VariableByteInteger(v) = value {
                    Some(*v)
                } else {
                    None
                }
            })
    }

    pub fn set_session_expiry_interval(&mut self, seconds: u32) {
        self.properties
            .entry(PropertyId::SessionExpiryInterval)
            .or_default()
            .push(PropertyValue::FourByteInteger(seconds));
    }

    #[must_use]
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

    pub fn set_assigned_client_identifier(&mut self, id: String) {
        self.properties
            .entry(PropertyId::AssignedClientIdentifier)
            .or_default()
            .push(PropertyValue::Utf8String(id));
    }

    pub fn set_server_keep_alive(&mut self, seconds: u16) {
        self.properties
            .entry(PropertyId::ServerKeepAlive)
            .or_default()
            .push(PropertyValue::TwoByteInteger(seconds));
    }

    pub fn set_authentication_method(&mut self, method: String) {
        self.properties
            .entry(PropertyId::AuthenticationMethod)
            .or_default()
            .push(PropertyValue::Utf8String(method));
    }

    pub fn set_authentication_data(&mut self, data: Bytes) {
        self.properties
            .entry(PropertyId::AuthenticationData)
            .or_default()
            .push(PropertyValue::BinaryData(data));
    }

    #[must_use]
    pub fn get_authentication_method(&self) -> Option<&String> {
        self.properties
            .get(&PropertyId::AuthenticationMethod)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::Utf8String(s) = value {
                    Some(s)
                } else {
                    None
                }
            })
    }

    #[must_use]
    pub fn get_authentication_data(&self) -> Option<&[u8]> {
        self.properties
            .get(&PropertyId::AuthenticationData)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::BinaryData(b) = value {
                    Some(b.as_ref())
                } else {
                    None
                }
            })
    }

    pub fn set_request_problem_information(&mut self, request: bool) {
        self.properties
            .entry(PropertyId::RequestProblemInformation)
            .or_default()
            .push(PropertyValue::Byte(u8::from(request)));
    }

    #[must_use]
    pub fn get_request_problem_information(&self) -> Option<bool> {
        self.properties
            .get(&PropertyId::RequestProblemInformation)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::Byte(v) = value {
                    Some(*v != 0)
                } else {
                    None
                }
            })
    }

    pub fn set_will_delay_interval(&mut self, seconds: u32) {
        self.properties
            .entry(PropertyId::WillDelayInterval)
            .or_default()
            .push(PropertyValue::FourByteInteger(seconds));
    }

    pub fn set_request_response_information(&mut self, request: bool) {
        self.properties
            .entry(PropertyId::RequestResponseInformation)
            .or_default()
            .push(PropertyValue::Byte(u8::from(request)));
    }

    #[must_use]
    pub fn get_request_response_information(&self) -> Option<bool> {
        self.properties
            .get(&PropertyId::RequestResponseInformation)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::Byte(v) = value {
                    Some(*v != 0)
                } else {
                    None
                }
            })
    }

    pub fn set_response_information(&mut self, info: String) {
        self.properties
            .entry(PropertyId::ResponseInformation)
            .or_default()
            .push(PropertyValue::Utf8String(info));
    }

    pub fn set_server_reference(&mut self, reference: String) {
        self.properties
            .entry(PropertyId::ServerReference)
            .or_default()
            .push(PropertyValue::Utf8String(reference));
    }

    pub fn set_reason_string(&mut self, reason: String) {
        self.properties
            .entry(PropertyId::ReasonString)
            .or_default()
            .push(PropertyValue::Utf8String(reason));
    }

    pub fn set_receive_maximum(&mut self, max: u16) {
        self.properties
            .entry(PropertyId::ReceiveMaximum)
            .or_default()
            .push(PropertyValue::TwoByteInteger(max));
    }

    #[must_use]
    pub fn get_receive_maximum(&self) -> Option<u16> {
        self.properties
            .get(&PropertyId::ReceiveMaximum)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::TwoByteInteger(v) = value {
                    Some(*v)
                } else {
                    None
                }
            })
    }

    pub fn set_topic_alias_maximum(&mut self, max: u16) {
        self.properties
            .entry(PropertyId::TopicAliasMaximum)
            .or_default()
            .push(PropertyValue::TwoByteInteger(max));
    }

    #[must_use]
    pub fn get_topic_alias_maximum(&self) -> Option<u16> {
        self.properties
            .get(&PropertyId::TopicAliasMaximum)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::TwoByteInteger(v) = value {
                    Some(*v)
                } else {
                    None
                }
            })
    }

    pub fn set_maximum_qos(&mut self, qos: u8) {
        self.properties
            .entry(PropertyId::MaximumQoS)
            .or_default()
            .push(PropertyValue::Byte(qos));
    }

    pub fn set_retain_available(&mut self, available: bool) {
        self.properties
            .entry(PropertyId::RetainAvailable)
            .or_default()
            .push(PropertyValue::Byte(u8::from(available)));
    }

    pub fn set_maximum_packet_size(&mut self, size: u32) {
        self.properties
            .entry(PropertyId::MaximumPacketSize)
            .or_default()
            .push(PropertyValue::FourByteInteger(size));
    }

    #[must_use]
    pub fn get_maximum_packet_size(&self) -> Option<u32> {
        self.properties
            .get(&PropertyId::MaximumPacketSize)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::FourByteInteger(v) = value {
                    Some(*v)
                } else {
                    None
                }
            })
    }

    pub fn set_wildcard_subscription_available(&mut self, available: bool) {
        self.properties
            .entry(PropertyId::WildcardSubscriptionAvailable)
            .or_default()
            .push(PropertyValue::Byte(u8::from(available)));
    }

    pub fn set_subscription_identifier_available(&mut self, available: bool) {
        self.properties
            .entry(PropertyId::SubscriptionIdentifierAvailable)
            .or_default()
            .push(PropertyValue::Byte(u8::from(available)));
    }

    pub fn set_shared_subscription_available(&mut self, available: bool) {
        self.properties
            .entry(PropertyId::SharedSubscriptionAvailable)
            .or_default()
            .push(PropertyValue::Byte(u8::from(available)));
    }

    #[must_use]
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

    pub fn set_content_type(&mut self, content_type: String) {
        self.properties
            .entry(PropertyId::ContentType)
            .or_default()
            .push(PropertyValue::Utf8String(content_type));
    }

    #[must_use]
    pub fn get_content_type(&self) -> Option<String> {
        self.properties
            .get(&PropertyId::ContentType)
            .and_then(|values| values.first())
            .and_then(|value| {
                if let PropertyValue::Utf8String(s) = value {
                    Some(s.clone())
                } else {
                    None
                }
            })
    }
}
