use mqtt5_protocol::connection::ReconnectConfig;
use mqtt5_protocol::protocol::v5::properties::{Properties, PropertyId, PropertyValue};
use mqtt5_protocol::time::Duration;
use mqtt5_protocol::types::MessageProperties;
use mqtt5_protocol::QoS;
#[cfg(feature = "codec")]
use std::rc::Rc;
use wasm_bindgen::prelude::*;

#[cfg(feature = "codec")]
use crate::codec::WasmCodecRegistry;

#[wasm_bindgen]
pub struct WasmReconnectOptions {
    pub(crate) enabled: bool,
    pub(crate) initial_delay_ms: u32,
    pub(crate) max_delay_ms: u32,
    pub(crate) backoff_factor: f64,
    pub(crate) max_attempts: Option<u32>,
}

#[wasm_bindgen]
#[allow(non_snake_case)]
impl WasmReconnectOptions {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::must_use_candidate)]
    pub fn new() -> Self {
        Self {
            enabled: true,
            initial_delay_ms: 1000,
            max_delay_ms: 60000,
            backoff_factor: 2.0,
            max_attempts: None,
        }
    }

    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::new()
        }
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    #[wasm_bindgen(setter)]
    pub fn set_enabled(&mut self, value: bool) {
        self.enabled = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn initialDelayMs(&self) -> u32 {
        self.initial_delay_ms
    }

    #[wasm_bindgen(setter)]
    pub fn set_initialDelayMs(&mut self, value: u32) {
        self.initial_delay_ms = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn maxDelayMs(&self) -> u32 {
        self.max_delay_ms
    }

    #[wasm_bindgen(setter)]
    pub fn set_maxDelayMs(&mut self, value: u32) {
        self.max_delay_ms = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn backoffFactor(&self) -> f64 {
        self.backoff_factor
    }

    #[wasm_bindgen(setter)]
    pub fn set_backoffFactor(&mut self, value: f64) {
        self.backoff_factor = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn maxAttempts(&self) -> Option<u32> {
        self.max_attempts
    }

    #[wasm_bindgen(setter)]
    pub fn set_maxAttempts(&mut self, value: Option<u32>) {
        self.max_attempts = value;
    }

    #[must_use]
    pub(crate) fn to_reconnect_config(&self) -> ReconnectConfig {
        let mut config = ReconnectConfig {
            enabled: self.enabled,
            initial_delay: Duration::from_millis(u64::from(self.initial_delay_ms)),
            max_delay: Duration::from_millis(u64::from(self.max_delay_ms)),
            backoff_factor_tenths: 20,
            max_attempts: self.max_attempts,
        };
        config.set_backoff_factor(self.backoff_factor);
        config
    }
}

impl Default for WasmReconnectOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for WasmReconnectOptions {
    fn clone(&self) -> Self {
        Self {
            enabled: self.enabled,
            initial_delay_ms: self.initial_delay_ms,
            max_delay_ms: self.max_delay_ms,
            backoff_factor: self.backoff_factor,
            max_attempts: self.max_attempts,
        }
    }
}

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
    pub(crate) protocol_version: u8,
    pub(crate) backup_urls: Vec<String>,
    #[cfg(feature = "codec")]
    pub(crate) codec_registry: Option<Rc<WasmCodecRegistry>>,
}

#[wasm_bindgen]
#[allow(non_snake_case)]
impl WasmConnectOptions {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::must_use_candidate)]
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
            protocol_version: 5,
            backup_urls: Vec::new(),
            #[cfg(feature = "codec")]
            codec_registry: None,
        }
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn keepAlive(&self) -> u16 {
        self.keep_alive
    }

    #[wasm_bindgen(setter)]
    pub fn set_keepAlive(&mut self, value: u16) {
        self.keep_alive = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn cleanStart(&self) -> bool {
        self.clean_start
    }

    #[wasm_bindgen(setter)]
    pub fn set_cleanStart(&mut self, value: bool) {
        self.clean_start = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
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
    #[must_use]
    pub fn sessionExpiryInterval(&self) -> Option<u32> {
        self.session_expiry_interval
    }

    #[wasm_bindgen(setter)]
    pub fn set_sessionExpiryInterval(&mut self, value: Option<u32>) {
        self.session_expiry_interval = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn receiveMaximum(&self) -> Option<u16> {
        self.receive_maximum
    }

    #[wasm_bindgen(setter)]
    pub fn set_receiveMaximum(&mut self, value: Option<u16>) {
        self.receive_maximum = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn maximumPacketSize(&self) -> Option<u32> {
        self.maximum_packet_size
    }

    #[wasm_bindgen(setter)]
    pub fn set_maximumPacketSize(&mut self, value: Option<u32>) {
        self.maximum_packet_size = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn topicAliasMaximum(&self) -> Option<u16> {
        self.topic_alias_maximum
    }

    #[wasm_bindgen(setter)]
    pub fn set_topicAliasMaximum(&mut self, value: Option<u16>) {
        self.topic_alias_maximum = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn requestResponseInformation(&self) -> Option<bool> {
        self.request_response_information
    }

    #[wasm_bindgen(setter)]
    pub fn set_requestResponseInformation(&mut self, value: Option<bool>) {
        self.request_response_information = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn requestProblemInformation(&self) -> Option<bool> {
        self.request_problem_information
    }

    #[wasm_bindgen(setter)]
    pub fn set_requestProblemInformation(&mut self, value: Option<bool>) {
        self.request_problem_information = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
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

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn protocolVersion(&self) -> u8 {
        self.protocol_version
    }

    #[wasm_bindgen(setter)]
    pub fn set_protocolVersion(&mut self, value: u8) {
        if value == 4 || value == 5 {
            self.protocol_version = value;
        } else {
            web_sys::console::warn_1(
                &"Protocol version must be 4 (v3.1.1) or 5 (v5.0). Using 5.".into(),
            );
            self.protocol_version = 5;
        }
    }

    pub fn addUserProperty(&mut self, key: String, value: String) {
        self.user_properties.push((key, value));
    }

    pub fn clearUserProperties(&mut self) {
        self.user_properties.clear();
    }

    pub fn addBackupUrl(&mut self, url: String) {
        self.backup_urls.push(url);
    }

    pub fn clearBackupUrls(&mut self) {
        self.backup_urls.clear();
    }

    #[must_use]
    pub fn getBackupUrls(&self) -> Vec<String> {
        self.backup_urls.clone()
    }

    #[cfg(feature = "codec")]
    #[wasm_bindgen(js_name = "setCodecRegistry")]
    pub fn set_codec_registry(&mut self, registry: WasmCodecRegistry) {
        self.codec_registry = Some(Rc::new(registry));
    }

    #[cfg(feature = "codec")]
    #[wasm_bindgen(js_name = "clearCodecRegistry")]
    pub fn clear_codec_registry(&mut self) {
        self.codec_registry = None;
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
    #[allow(clippy::must_use_candidate)]
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
    #[must_use]
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
    #[must_use]
    pub fn retain(&self) -> bool {
        self.retain
    }

    #[wasm_bindgen(setter)]
    pub fn set_retain(&mut self, value: bool) {
        self.retain = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn payloadFormatIndicator(&self) -> Option<bool> {
        self.payload_format_indicator
    }

    #[wasm_bindgen(setter)]
    pub fn set_payloadFormatIndicator(&mut self, value: Option<bool>) {
        self.payload_format_indicator = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn messageExpiryInterval(&self) -> Option<u32> {
        self.message_expiry_interval
    }

    #[wasm_bindgen(setter)]
    pub fn set_messageExpiryInterval(&mut self, value: Option<u32>) {
        self.message_expiry_interval = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn topicAlias(&self) -> Option<u16> {
        self.topic_alias
    }

    #[wasm_bindgen(setter)]
    pub fn set_topicAlias(&mut self, value: Option<u16>) {
        self.topic_alias = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
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
    #[must_use]
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
    #[allow(clippy::must_use_candidate)]
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
    #[must_use]
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
    #[must_use]
    pub fn noLocal(&self) -> bool {
        self.no_local
    }

    #[wasm_bindgen(setter)]
    pub fn set_noLocal(&mut self, value: bool) {
        self.no_local = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn retainAsPublished(&self) -> bool {
        self.retain_as_published
    }

    #[wasm_bindgen(setter)]
    pub fn set_retainAsPublished(&mut self, value: bool) {
        self.retain_as_published = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
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
    #[must_use]
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
    #[allow(clippy::must_use_candidate)]
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
    #[must_use]
    pub fn topic(&self) -> String {
        self.topic.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_topic(&mut self, value: String) {
        self.topic = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
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
    #[must_use]
    pub fn retain(&self) -> bool {
        self.retain
    }

    #[wasm_bindgen(setter)]
    pub fn set_retain(&mut self, value: bool) {
        self.retain = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn willDelayInterval(&self) -> Option<u32> {
        self.will_delay_interval
    }

    #[wasm_bindgen(setter)]
    pub fn set_willDelayInterval(&mut self, value: Option<u32>) {
        self.will_delay_interval = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn messageExpiryInterval(&self) -> Option<u32> {
        self.message_expiry_interval
    }

    #[wasm_bindgen(setter)]
    pub fn set_messageExpiryInterval(&mut self, value: Option<u32>) {
        self.message_expiry_interval = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn contentType(&self) -> Option<String> {
        self.content_type.clone()
    }

    #[wasm_bindgen(setter)]
    pub fn set_contentType(&mut self, value: Option<String>) {
        self.content_type = value;
    }

    #[wasm_bindgen(getter)]
    #[must_use]
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

#[wasm_bindgen]
pub struct WasmMessageProperties {
    response_topic: Option<String>,
    correlation_data: Option<Vec<u8>>,
    content_type: Option<String>,
    payload_format_indicator: Option<bool>,
    message_expiry_interval: Option<u32>,
    subscription_identifiers: Vec<u32>,
    user_properties: Vec<(String, String)>,
}

#[wasm_bindgen]
#[allow(non_snake_case)]
impl WasmMessageProperties {
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn responseTopic(&self) -> Option<String> {
        self.response_topic.clone()
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn correlationData(&self) -> Option<Vec<u8>> {
        self.correlation_data.clone()
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn contentType(&self) -> Option<String> {
        self.content_type.clone()
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn payloadFormatIndicator(&self) -> Option<bool> {
        self.payload_format_indicator
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn messageExpiryInterval(&self) -> Option<u32> {
        self.message_expiry_interval
    }

    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn subscriptionIdentifiers(&self) -> Vec<u32> {
        self.subscription_identifiers.clone()
    }

    #[must_use]
    pub fn getUserProperties(&self) -> js_sys::Array {
        let arr = js_sys::Array::new();
        for (key, value) in &self.user_properties {
            let pair = js_sys::Array::new();
            pair.push(&JsValue::from_str(key));
            pair.push(&JsValue::from_str(value));
            arr.push(&pair);
        }
        arr
    }
}

impl From<MessageProperties> for WasmMessageProperties {
    fn from(props: MessageProperties) -> Self {
        Self {
            response_topic: props.response_topic,
            correlation_data: props.correlation_data,
            content_type: props.content_type,
            payload_format_indicator: props.payload_format_indicator,
            message_expiry_interval: props.message_expiry_interval,
            subscription_identifiers: props.subscription_identifiers,
            user_properties: props.user_properties,
        }
    }
}
