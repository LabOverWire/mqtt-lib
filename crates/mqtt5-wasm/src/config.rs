use mqtt5_protocol::protocol::v5::properties::{Properties, PropertyId, PropertyValue};
use mqtt5_protocol::QoS;
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub struct WasmConnectOptions {
    pub(crate) keep_alive: u16,
    pub(crate) clean_start: bool,
    pub(crate) username: Option<String>,
    pub(crate) password: Option<Vec<u8>>,
    pub(crate) will: Option<WasmWillMessage>,
    pub(crate) session_expiry_interval: Option<u32>,
    pub(crate) receive_maximum: Option<u16>,
    pub(crate) maximum_packet_size: Option<u32>,
    pub(crate) topic_alias_maximum: Option<u16>,
    pub(crate) request_response_information: Option<bool>,
    pub(crate) request_problem_information: Option<bool>,
    pub(crate) authentication_method: Option<String>,
    pub(crate) authentication_data: Option<Vec<u8>>,
    pub(crate) user_properties: Vec<(String, String)>,
}

#[wasm_bindgen]
#[allow(non_snake_case)]
impl WasmConnectOptions {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            keep_alive: 60,
            clean_start: true,
            username: None,
            password: None,
            will: None,
            session_expiry_interval: None,
            receive_maximum: None,
            maximum_packet_size: None,
            topic_alias_maximum: None,
            request_response_information: None,
            request_problem_information: None,
            authentication_method: None,
            authentication_data: None,
            user_properties: Vec::new(),
        }
    }

    #[wasm_bindgen(getter)]
    pub fn keepAlive(&self) -> u16 {
        self.keep_alive
    }

    #[wasm_bindgen(setter)]
    pub fn set_keepAlive(&mut self, value: u16) {
        self.keep_alive = value;
    }

    #[wasm_bindgen(getter)]
    pub fn cleanStart(&self) -> bool {
        self.clean_start
    }

    #[wasm_bindgen(setter)]
    pub fn set_cleanStart(&mut self, value: bool) {
        self.clean_start = value;
    }

    #[wasm_bindgen(getter)]
    pub fn username(&self) -> Option<String> {
        self.username.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_username(&mut self, value: Option<String>) {
        self.username = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_password(&mut self, value: &[u8]) {
        self.password = Some(value.to_vec());
    }

    pub fn set_will(&mut self, will: WasmWillMessage) {
        self.will = Some(will);
    }

    pub fn clear_will(&mut self) {
        self.will = None;
    }

    #[wasm_bindgen(getter)]
    pub fn sessionExpiryInterval(&self) -> Option<u32> {
        self.session_expiry_interval
    }

    #[wasm_bindgen(setter)]
    pub fn set_sessionExpiryInterval(&mut self, value: Option<u32>) {
        self.session_expiry_interval = value;
    }

    #[wasm_bindgen(getter)]
    pub fn receiveMaximum(&self) -> Option<u16> {
        self.receive_maximum
    }

    #[wasm_bindgen(setter)]
    pub fn set_receiveMaximum(&mut self, value: Option<u16>) {
        self.receive_maximum = value;
    }

    #[wasm_bindgen(getter)]
    pub fn maximumPacketSize(&self) -> Option<u32> {
        self.maximum_packet_size
    }

    #[wasm_bindgen(setter)]
    pub fn set_maximumPacketSize(&mut self, value: Option<u32>) {
        self.maximum_packet_size = value;
    }

    #[wasm_bindgen(getter)]
    pub fn topicAliasMaximum(&self) -> Option<u16> {
        self.topic_alias_maximum
    }

    #[wasm_bindgen(setter)]
    pub fn set_topicAliasMaximum(&mut self, value: Option<u16>) {
        self.topic_alias_maximum = value;
    }

    #[wasm_bindgen(getter)]
    pub fn requestResponseInformation(&self) -> Option<bool> {
        self.request_response_information
    }

    #[wasm_bindgen(setter)]
    pub fn set_requestResponseInformation(&mut self, value: Option<bool>) {
        self.request_response_information = value;
    }

    #[wasm_bindgen(getter)]
    pub fn requestProblemInformation(&self) -> Option<bool> {
        self.request_problem_information
    }

    #[wasm_bindgen(setter)]
    pub fn set_requestProblemInformation(&mut self, value: Option<bool>) {
        self.request_problem_information = value;
    }

    #[wasm_bindgen(getter)]
    pub fn authenticationMethod(&self) -> Option<String> {
        self.authentication_method.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_authenticationMethod(&mut self, value: Option<String>) {
        self.authentication_method = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_authenticationData(&mut self, value: &[u8]) {
        self.authentication_data = Some(value.to_vec());
    }

    pub fn addUserProperty(&mut self, key: String, value: String) {
        self.user_properties.push((key, value));
    }

    pub fn clearUserProperties(&mut self) {
        self.user_properties.clear();
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) fn to_properties(&self) -> Properties {
        let mut properties = Properties::default();

        if let Some(interval) = self.session_expiry_interval {
            if properties
                .add(
                    PropertyId::SessionExpiryInterval,
                    PropertyValue::FourByteInteger(interval),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add session expiry interval property".into());
            }
        }

        if let Some(max) = self.receive_maximum {
            if properties
                .add(
                    PropertyId::ReceiveMaximum,
                    PropertyValue::TwoByteInteger(max),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add receive maximum property".into());
            }
        }

        if let Some(size) = self.maximum_packet_size {
            if properties
                .add(
                    PropertyId::MaximumPacketSize,
                    PropertyValue::FourByteInteger(size),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add maximum packet size property".into());
            }
        }

        if let Some(max) = self.topic_alias_maximum {
            if properties
                .add(
                    PropertyId::TopicAliasMaximum,
                    PropertyValue::TwoByteInteger(max),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add topic alias maximum property".into());
            }
        }

        if let Some(val) = self.request_response_information {
            if properties
                .add(
                    PropertyId::RequestResponseInformation,
                    PropertyValue::Byte(u8::from(val)),
                )
                .is_err()
            {
                web_sys::console::warn_1(
                    &"Failed to add request response information property".into(),
                );
            }
        }

        if let Some(val) = self.request_problem_information {
            if properties
                .add(
                    PropertyId::RequestProblemInformation,
                    PropertyValue::Byte(u8::from(val)),
                )
                .is_err()
            {
                web_sys::console::warn_1(
                    &"Failed to add request problem information property".into(),
                );
            }
        }

        if let Some(method) = &self.authentication_method {
            if properties
                .add(
                    PropertyId::AuthenticationMethod,
                    PropertyValue::Utf8String(method.clone()),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add authentication method property".into());
            }
        }

        if let Some(data) = &self.authentication_data {
            if properties
                .add(
                    PropertyId::AuthenticationData,
                    PropertyValue::BinaryData(data.clone().into()),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add authentication data property".into());
            }
        }

        for (key, value) in &self.user_properties {
            if properties
                .add(
                    PropertyId::UserProperty,
                    PropertyValue::Utf8StringPair(key.clone(), value.clone()),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add user property".into());
            }
        }

        properties
    }
}

impl Default for WasmConnectOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
pub struct WasmPublishOptions {
    pub(crate) qos: u8,
    pub(crate) retain: bool,
    pub(crate) payload_format_indicator: Option<bool>,
    pub(crate) message_expiry_interval: Option<u32>,
    pub(crate) topic_alias: Option<u16>,
    pub(crate) response_topic: Option<String>,
    pub(crate) correlation_data: Option<Vec<u8>>,
    pub(crate) content_type: Option<String>,
    pub(crate) user_properties: Vec<(String, String)>,
}

#[wasm_bindgen]
#[allow(non_snake_case)]
impl WasmPublishOptions {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            qos: 0,
            retain: false,
            payload_format_indicator: None,
            message_expiry_interval: None,
            topic_alias: None,
            response_topic: None,
            correlation_data: None,
            content_type: None,
            user_properties: Vec::new(),
        }
    }

    #[wasm_bindgen(getter)]
    pub fn qos(&self) -> u8 {
        self.qos
    }

    #[wasm_bindgen(setter)]
    pub fn set_qos(&mut self, value: u8) {
        if value > 2 {
            web_sys::console::warn_1(&"QoS must be 0, 1, or 2. Using 0.".into());
            self.qos = 0;
        } else {
            self.qos = value;
        }
    }

    #[wasm_bindgen(getter)]
    pub fn retain(&self) -> bool {
        self.retain
    }

    #[wasm_bindgen(setter)]
    pub fn set_retain(&mut self, value: bool) {
        self.retain = value;
    }

    #[wasm_bindgen(getter)]
    pub fn payloadFormatIndicator(&self) -> Option<bool> {
        self.payload_format_indicator
    }

    #[wasm_bindgen(setter)]
    pub fn set_payloadFormatIndicator(&mut self, value: Option<bool>) {
        self.payload_format_indicator = value;
    }

    #[wasm_bindgen(getter)]
    pub fn messageExpiryInterval(&self) -> Option<u32> {
        self.message_expiry_interval
    }

    #[wasm_bindgen(setter)]
    pub fn set_messageExpiryInterval(&mut self, value: Option<u32>) {
        self.message_expiry_interval = value;
    }

    #[wasm_bindgen(getter)]
    pub fn topicAlias(&self) -> Option<u16> {
        self.topic_alias
    }

    #[wasm_bindgen(setter)]
    pub fn set_topicAlias(&mut self, value: Option<u16>) {
        self.topic_alias = value;
    }

    #[wasm_bindgen(getter)]
    pub fn responseTopic(&self) -> Option<String> {
        self.response_topic.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_responseTopic(&mut self, value: Option<String>) {
        self.response_topic = value;
    }

    #[wasm_bindgen(setter)]
    pub fn set_correlationData(&mut self, value: &[u8]) {
        self.correlation_data = Some(value.to_vec());
    }

    #[wasm_bindgen(getter)]
    pub fn contentType(&self) -> Option<String> {
        self.content_type.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_contentType(&mut self, value: Option<String>) {
        self.content_type = value;
    }

    pub fn addUserProperty(&mut self, key: String, value: String) {
        self.user_properties.push((key, value));
    }

    pub fn clearUserProperties(&mut self) {
        self.user_properties.clear();
    }

    pub(crate) fn to_qos(&self) -> QoS {
        match self.qos {
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => QoS::AtMostOnce,
        }
    }

    pub(crate) fn to_properties(&self) -> Properties {
        let mut properties = Properties::default();

        if let Some(val) = self.payload_format_indicator {
            if properties
                .add(
                    PropertyId::PayloadFormatIndicator,
                    PropertyValue::Byte(u8::from(val)),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add payload format indicator property".into());
            }
        }

        if let Some(val) = self.message_expiry_interval {
            if properties
                .add(
                    PropertyId::MessageExpiryInterval,
                    PropertyValue::FourByteInteger(val),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add message expiry interval property".into());
            }
        }

        if let Some(val) = self.topic_alias {
            if properties
                .add(PropertyId::TopicAlias, PropertyValue::TwoByteInteger(val))
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add topic alias property".into());
            }
        }

        if let Some(val) = &self.response_topic {
            if properties
                .add(
                    PropertyId::ResponseTopic,
                    PropertyValue::Utf8String(val.clone()),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add response topic property".into());
            }
        }

        if let Some(val) = &self.correlation_data {
            if properties
                .add(
                    PropertyId::CorrelationData,
                    PropertyValue::BinaryData(val.clone().into()),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add correlation data property".into());
            }
        }

        if let Some(val) = &self.content_type {
            if properties
                .add(
                    PropertyId::ContentType,
                    PropertyValue::Utf8String(val.clone()),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add content type property".into());
            }
        }

        for (key, value) in &self.user_properties {
            if properties
                .add(
                    PropertyId::UserProperty,
                    PropertyValue::Utf8StringPair(key.clone(), value.clone()),
                )
                .is_err()
            {
                web_sys::console::warn_1(&"Failed to add user property".into());
            }
        }

        properties
    }
}

impl Default for WasmPublishOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
pub struct WasmSubscribeOptions {
    pub(crate) qos: u8,
    pub(crate) no_local: bool,
    pub(crate) retain_as_published: bool,
    pub(crate) retain_handling: u8,
    pub(crate) subscription_identifier: Option<u32>,
}

#[wasm_bindgen]
#[allow(non_snake_case)]
impl WasmSubscribeOptions {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            qos: 0,
            no_local: false,
            retain_as_published: false,
            retain_handling: 0,
            subscription_identifier: None,
        }
    }

    #[wasm_bindgen(getter)]
    pub fn qos(&self) -> u8 {
        self.qos
    }

    #[wasm_bindgen(setter)]
    pub fn set_qos(&mut self, value: u8) {
        if value > 2 {
            web_sys::console::warn_1(&"QoS must be 0, 1, or 2. Using 0.".into());
            self.qos = 0;
        } else {
            self.qos = value;
        }
    }

    #[wasm_bindgen(getter)]
    pub fn noLocal(&self) -> bool {
        self.no_local
    }

    #[wasm_bindgen(setter)]
    pub fn set_noLocal(&mut self, value: bool) {
        self.no_local = value;
    }

    #[wasm_bindgen(getter)]
    pub fn retainAsPublished(&self) -> bool {
        self.retain_as_published
    }

    #[wasm_bindgen(setter)]
    pub fn set_retainAsPublished(&mut self, value: bool) {
        self.retain_as_published = value;
    }

    #[wasm_bindgen(getter)]
    pub fn retainHandling(&self) -> u8 {
        self.retain_handling
    }

    #[wasm_bindgen(setter)]
    pub fn set_retainHandling(&mut self, value: u8) {
        if value > 2 {
            web_sys::console::warn_1(&"Retain handling must be 0, 1, or 2. Using 0.".into());
            self.retain_handling = 0;
        } else {
            self.retain_handling = value;
        }
    }

    #[wasm_bindgen(getter)]
    pub fn subscriptionIdentifier(&self) -> Option<u32> {
        self.subscription_identifier
    }

    #[wasm_bindgen(setter)]
    pub fn set_subscriptionIdentifier(&mut self, value: Option<u32>) {
        self.subscription_identifier = value;
    }

    pub(crate) fn to_qos(&self) -> QoS {
        match self.qos {
            1 => QoS::AtLeastOnce,
            2 => QoS::ExactlyOnce,
            _ => QoS::AtMostOnce,
        }
    }
}

impl Default for WasmSubscribeOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
pub struct WasmWillMessage {
    pub(crate) topic: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) qos: u8,
    pub(crate) retain: bool,
    pub(crate) will_delay_interval: Option<u32>,
    pub(crate) message_expiry_interval: Option<u32>,
    pub(crate) content_type: Option<String>,
    pub(crate) response_topic: Option<String>,
}

#[wasm_bindgen]
#[allow(non_snake_case)]
impl WasmWillMessage {
    #[wasm_bindgen(constructor)]
    pub fn new(topic: String, payload: Vec<u8>) -> Self {
        Self {
            topic,
            payload,
            qos: 0,
            retain: false,
            will_delay_interval: None,
            message_expiry_interval: None,
            content_type: None,
            response_topic: None,
        }
    }

    #[wasm_bindgen(getter)]
    pub fn topic(&self) -> String {
        self.topic.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_topic(&mut self, value: String) {
        self.topic = value;
    }

    #[wasm_bindgen(getter)]
    pub fn qos(&self) -> u8 {
        self.qos
    }

    #[wasm_bindgen(setter)]
    pub fn set_qos(&mut self, value: u8) {
        if value > 2 {
            web_sys::console::warn_1(&"QoS must be 0, 1, or 2. Using 0.".into());
            self.qos = 0;
        } else {
            self.qos = value;
        }
    }

    #[wasm_bindgen(getter)]
    pub fn retain(&self) -> bool {
        self.retain
    }

    #[wasm_bindgen(setter)]
    pub fn set_retain(&mut self, value: bool) {
        self.retain = value;
    }

    #[wasm_bindgen(getter)]
    pub fn willDelayInterval(&self) -> Option<u32> {
        self.will_delay_interval
    }

    #[wasm_bindgen(setter)]
    pub fn set_willDelayInterval(&mut self, value: Option<u32>) {
        self.will_delay_interval = value;
    }

    #[wasm_bindgen(getter)]
    pub fn messageExpiryInterval(&self) -> Option<u32> {
        self.message_expiry_interval
    }

    #[wasm_bindgen(setter)]
    pub fn set_messageExpiryInterval(&mut self, value: Option<u32>) {
        self.message_expiry_interval = value;
    }

    #[wasm_bindgen(getter)]
    pub fn contentType(&self) -> Option<String> {
        self.content_type.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_contentType(&mut self, value: Option<String>) {
        self.content_type = value;
    }

    #[wasm_bindgen(getter)]
    pub fn responseTopic(&self) -> Option<String> {
        self.response_topic.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_responseTopic(&mut self, value: Option<String>) {
        self.response_topic = value;
    }

    pub(crate) fn to_will_message(&self) -> mqtt5_protocol::types::WillMessage {
        let mut will = mqtt5_protocol::types::WillMessage {
            topic: self.topic.clone(),
            payload: self.payload.clone(),
            qos: match self.qos {
                1 => QoS::AtLeastOnce,
                2 => QoS::ExactlyOnce,
                _ => QoS::AtMostOnce,
            },
            retain: self.retain,
            properties: mqtt5_protocol::types::WillProperties::default(),
        };

        will.properties.will_delay_interval = self.will_delay_interval;
        will.properties.message_expiry_interval = self.message_expiry_interval;
        will.properties.content_type.clone_from(&self.content_type);
        will.properties
            .response_topic
            .clone_from(&self.response_topic);

        will
    }
}
