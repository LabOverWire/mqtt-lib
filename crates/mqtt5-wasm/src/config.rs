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

fn add_property_or_warn(properties: &mut Properties, id: PropertyId, value: PropertyValue) {
    if properties.add(id, value).is_err() {
        web_sys::console::warn_1(&format!("Failed to add {id:?} property").into());
    }
}

#[wasm_bindgen(js_name = "ReconnectOptions")]
pub struct WasmReconnectOptions {
    pub(crate) enabled: bool,
    pub(crate) initial_delay_ms: u32,
    pub(crate) max_delay_ms: u32,
    pub(crate) backoff_factor: f64,
    pub(crate) max_attempts: Option<u32>,
}

#[wasm_bindgen(js_class = "ReconnectOptions")]
impl WasmReconnectOptions {
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            enabled: true,
            initial_delay_ms: 1000,
            max_delay_ms: 60000,
            backoff_factor: 2.0,
            max_attempts: Some(20),
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

    #[wasm_bindgen(getter = initialDelayMs)]
    #[must_use]
    pub fn initial_delay_ms(&self) -> u32 {
        self.initial_delay_ms
    }

    #[wasm_bindgen(setter = initialDelayMs)]
    pub fn set_initial_delay_ms(&mut self, value: u32) {
        self.initial_delay_ms = value;
    }

    #[wasm_bindgen(getter = maxDelayMs)]
    #[must_use]
    pub fn max_delay_ms(&self) -> u32 {
        self.max_delay_ms
    }

    #[wasm_bindgen(setter = maxDelayMs)]
    pub fn set_max_delay_ms(&mut self, value: u32) {
        self.max_delay_ms = value;
    }

    #[wasm_bindgen(getter = backoffFactor)]
    #[must_use]
    pub fn backoff_factor(&self) -> f64 {
        self.backoff_factor
    }

    #[wasm_bindgen(setter = backoffFactor)]
    pub fn set_backoff_factor(&mut self, value: f64) {
        self.backoff_factor = value;
    }

    #[wasm_bindgen(getter = maxAttempts)]
    #[must_use]
    pub fn max_attempts(&self) -> Option<u32> {
        self.max_attempts
    }

    #[wasm_bindgen(setter = maxAttempts)]
    pub fn set_max_attempts(&mut self, value: Option<u32>) {
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

#[wasm_bindgen(js_name = "ConnectOptions")]
pub struct WasmConnectOptions {
    pub(crate) keep_alive: u16,
    pub(crate) clean_start: bool,
    pub(crate) resume_existing_session: bool,
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

#[wasm_bindgen(js_class = "ConnectOptions")]
impl WasmConnectOptions {
    #[wasm_bindgen(constructor)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            keep_alive: 60,
            clean_start: true,
            resume_existing_session: false,
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

    #[wasm_bindgen(getter = keepAlive)]
    #[must_use]
    pub fn keep_alive(&self) -> u16 {
        self.keep_alive
    }

    #[wasm_bindgen(setter = keepAlive)]
    pub fn set_keep_alive(&mut self, value: u16) {
        self.keep_alive = value;
    }

    #[wasm_bindgen(getter = cleanStart)]
    #[must_use]
    pub fn clean_start(&self) -> bool {
        self.clean_start
    }

    #[wasm_bindgen(setter = cleanStart)]
    pub fn set_clean_start(&mut self, value: bool) {
        self.clean_start = value;
    }

    /// Accept `Session Present = 1` on a client that holds no local session state.
    ///
    /// By default a client that has not yet established a session in this instance
    /// rejects a CONNACK with Session Present set to 1, sends DISCONNECT 0x82 and closes
    /// the connection (MQTT-3.2.2-4). Set this to `true` (together with
    /// `cleanStart = false`) to deliberately resume a session the broker still holds,
    /// for example after a page reload or crash. The client has nothing to resend in
    /// that case; the broker resumes delivery of the messages it holds. A CONNACK with
    /// Session Present set to 1 in reply to `cleanStart = true` is always rejected.
    #[wasm_bindgen(getter = resumeExistingSession)]
    #[must_use]
    pub fn resume_existing_session(&self) -> bool {
        self.resume_existing_session
    }

    /// Sets [`Self::resume_existing_session`].
    #[wasm_bindgen(setter = resumeExistingSession)]
    pub fn set_resume_existing_session(&mut self, value: bool) {
        self.resume_existing_session = value;
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

    #[wasm_bindgen(js_name = "setWill")]
    pub fn set_will(&mut self, will: WasmWillMessage) {
        self.will = Some(will);
    }

    #[wasm_bindgen(js_name = "clearWill")]
    pub fn clear_will(&mut self) {
        self.will = None;
    }

    #[wasm_bindgen(getter = sessionExpiryInterval)]
    #[must_use]
    pub fn session_expiry_interval(&self) -> Option<u32> {
        self.session_expiry_interval
    }

    #[wasm_bindgen(setter = sessionExpiryInterval)]
    pub fn set_session_expiry_interval(&mut self, value: Option<u32>) {
        self.session_expiry_interval = value;
    }

    #[wasm_bindgen(getter = receiveMaximum)]
    #[must_use]
    pub fn receive_maximum(&self) -> Option<u16> {
        self.receive_maximum
    }

    #[wasm_bindgen(setter = receiveMaximum)]
    pub fn set_receive_maximum(&mut self, value: Option<u16>) {
        self.receive_maximum = value;
    }

    #[wasm_bindgen(getter = maximumPacketSize)]
    #[must_use]
    pub fn maximum_packet_size(&self) -> Option<u32> {
        self.maximum_packet_size
    }

    #[wasm_bindgen(setter = maximumPacketSize)]
    pub fn set_maximum_packet_size(&mut self, value: Option<u32>) {
        self.maximum_packet_size = value;
    }

    #[wasm_bindgen(getter = topicAliasMaximum)]
    #[must_use]
    pub fn topic_alias_maximum(&self) -> Option<u16> {
        self.topic_alias_maximum
    }

    #[wasm_bindgen(setter = topicAliasMaximum)]
    pub fn set_topic_alias_maximum(&mut self, value: Option<u16>) {
        self.topic_alias_maximum = value;
    }

    #[wasm_bindgen(getter = requestResponseInformation)]
    #[must_use]
    pub fn request_response_information(&self) -> Option<bool> {
        self.request_response_information
    }

    #[wasm_bindgen(setter = requestResponseInformation)]
    pub fn set_request_response_information(&mut self, value: Option<bool>) {
        self.request_response_information = value;
    }

    #[wasm_bindgen(getter = requestProblemInformation)]
    #[must_use]
    pub fn request_problem_information(&self) -> Option<bool> {
        self.request_problem_information
    }

    #[wasm_bindgen(setter = requestProblemInformation)]
    pub fn set_request_problem_information(&mut self, value: Option<bool>) {
        self.request_problem_information = value;
    }

    #[wasm_bindgen(getter = authenticationMethod)]
    #[must_use]
    pub fn authentication_method(&self) -> Option<String> {
        self.authentication_method.clone()
    }

    #[wasm_bindgen(setter = authenticationMethod)]
    pub fn set_authentication_method(&mut self, value: Option<String>) {
        self.authentication_method = value;
    }

    #[wasm_bindgen(setter = authenticationData)]
    pub fn set_authentication_data(&mut self, value: &[u8]) {
        self.authentication_data = Some(value.to_vec());
    }

    #[wasm_bindgen(getter = protocolVersion)]
    #[must_use]
    pub fn protocol_version(&self) -> u8 {
        self.protocol_version
    }

    #[wasm_bindgen(setter = protocolVersion)]
    pub fn set_protocol_version(&mut self, value: u8) {
        if value == 4 || value == 5 {
            self.protocol_version = value;
        } else {
            web_sys::console::warn_1(
                &"Protocol version must be 4 (v3.1.1) or 5 (v5.0). Using 5.".into(),
            );
            self.protocol_version = 5;
        }
    }

    #[wasm_bindgen(js_name = addUserProperty)]
    pub fn add_user_property(&mut self, key: String, value: String) {
        self.user_properties.push((key, value));
    }

    #[wasm_bindgen(js_name = clearUserProperties)]
    pub fn clear_user_properties(&mut self) {
        self.user_properties.clear();
    }

    #[wasm_bindgen(js_name = addBackupUrl)]
    pub fn add_backup_url(&mut self, url: String) {
        self.backup_urls.push(url);
    }

    #[wasm_bindgen(js_name = clearBackupUrls)]
    pub fn clear_backup_urls(&mut self) {
        self.backup_urls.clear();
    }

    #[must_use]
    #[wasm_bindgen(js_name = getBackupUrls)]
    pub fn get_backup_urls(&self) -> Vec<String> {
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

    pub(crate) fn to_properties(&self) -> Properties {
        let mut properties = Properties::default();

        if let Some(interval) = self.session_expiry_interval {
            add_property_or_warn(
                &mut properties,
                PropertyId::SessionExpiryInterval,
                PropertyValue::FourByteInteger(interval),
            );
        }

        if let Some(max) = self.receive_maximum {
            add_property_or_warn(
                &mut properties,
                PropertyId::ReceiveMaximum,
                PropertyValue::TwoByteInteger(max),
            );
        }

        if let Some(size) = self.maximum_packet_size {
            add_property_or_warn(
                &mut properties,
                PropertyId::MaximumPacketSize,
                PropertyValue::FourByteInteger(size),
            );
        }

        if let Some(max) = self.topic_alias_maximum {
            add_property_or_warn(
                &mut properties,
                PropertyId::TopicAliasMaximum,
                PropertyValue::TwoByteInteger(max),
            );
        }

        if let Some(val) = self.request_response_information {
            add_property_or_warn(
                &mut properties,
                PropertyId::RequestResponseInformation,
                PropertyValue::Byte(u8::from(val)),
            );
        }

        if let Some(val) = self.request_problem_information {
            add_property_or_warn(
                &mut properties,
                PropertyId::RequestProblemInformation,
                PropertyValue::Byte(u8::from(val)),
            );
        }

        if let Some(method) = &self.authentication_method {
            add_property_or_warn(
                &mut properties,
                PropertyId::AuthenticationMethod,
                PropertyValue::Utf8String(method.clone()),
            );
        }

        if let Some(data) = &self.authentication_data {
            add_property_or_warn(
                &mut properties,
                PropertyId::AuthenticationData,
                PropertyValue::BinaryData(data.clone().into()),
            );
        }

        for (key, value) in &self.user_properties {
            add_property_or_warn(
                &mut properties,
                PropertyId::UserProperty,
                PropertyValue::Utf8StringPair(key.clone(), value.clone()),
            );
        }

        properties
    }
}

impl Default for WasmConnectOptions {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen(js_name = "PublishOptions")]
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

#[wasm_bindgen(js_class = "PublishOptions")]
impl WasmPublishOptions {
    #[wasm_bindgen(constructor)]
    #[must_use]
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

    #[wasm_bindgen(getter = payloadFormatIndicator)]
    #[must_use]
    pub fn payload_format_indicator(&self) -> Option<bool> {
        self.payload_format_indicator
    }

    #[wasm_bindgen(setter = payloadFormatIndicator)]
    pub fn set_payload_format_indicator(&mut self, value: Option<bool>) {
        self.payload_format_indicator = value;
    }

    #[wasm_bindgen(getter = messageExpiryInterval)]
    #[must_use]
    pub fn message_expiry_interval(&self) -> Option<u32> {
        self.message_expiry_interval
    }

    #[wasm_bindgen(setter = messageExpiryInterval)]
    pub fn set_message_expiry_interval(&mut self, value: Option<u32>) {
        self.message_expiry_interval = value;
    }

    #[wasm_bindgen(getter = topicAlias)]
    #[must_use]
    pub fn topic_alias(&self) -> Option<u16> {
        self.topic_alias
    }

    #[wasm_bindgen(setter = topicAlias)]
    pub fn set_topic_alias(&mut self, value: Option<u16>) {
        self.topic_alias = value;
    }

    #[wasm_bindgen(getter = responseTopic)]
    #[must_use]
    pub fn response_topic(&self) -> Option<String> {
        self.response_topic.clone()
    }

    #[wasm_bindgen(setter = responseTopic)]
    pub fn set_response_topic(&mut self, value: Option<String>) {
        self.response_topic = value;
    }

    #[wasm_bindgen(setter = correlationData)]
    pub fn set_correlation_data(&mut self, value: &[u8]) {
        self.correlation_data = Some(value.to_vec());
    }

    #[wasm_bindgen(getter = contentType)]
    #[must_use]
    pub fn content_type(&self) -> Option<String> {
        self.content_type.clone()
    }

    #[wasm_bindgen(setter = contentType)]
    pub fn set_content_type(&mut self, value: Option<String>) {
        self.content_type = value;
    }

    #[wasm_bindgen(js_name = addUserProperty)]
    pub fn add_user_property(&mut self, key: String, value: String) {
        self.user_properties.push((key, value));
    }

    #[wasm_bindgen(js_name = clearUserProperties)]
    pub fn clear_user_properties(&mut self) {
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

#[wasm_bindgen(js_name = "SubscribeOptions")]
pub struct WasmSubscribeOptions {
    pub(crate) qos: u8,
    pub(crate) no_local: bool,
    pub(crate) retain_as_published: bool,
    pub(crate) retain_handling: u8,
    pub(crate) subscription_identifier: Option<u32>,
}

#[wasm_bindgen(js_class = "SubscribeOptions")]
impl WasmSubscribeOptions {
    #[wasm_bindgen(constructor)]
    #[must_use]
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

    #[wasm_bindgen(getter = noLocal)]
    #[must_use]
    pub fn no_local(&self) -> bool {
        self.no_local
    }

    #[wasm_bindgen(setter = noLocal)]
    pub fn set_no_local(&mut self, value: bool) {
        self.no_local = value;
    }

    #[wasm_bindgen(getter = retainAsPublished)]
    #[must_use]
    pub fn retain_as_published(&self) -> bool {
        self.retain_as_published
    }

    #[wasm_bindgen(setter = retainAsPublished)]
    pub fn set_retain_as_published(&mut self, value: bool) {
        self.retain_as_published = value;
    }

    #[wasm_bindgen(getter = retainHandling)]
    #[must_use]
    pub fn retain_handling(&self) -> u8 {
        self.retain_handling
    }

    #[wasm_bindgen(setter = retainHandling)]
    pub fn set_retain_handling(&mut self, value: u8) {
        if value > 2 {
            web_sys::console::warn_1(&"Retain handling must be 0, 1, or 2. Using 0.".into());
            self.retain_handling = 0;
        } else {
            self.retain_handling = value;
        }
    }

    #[wasm_bindgen(getter = subscriptionIdentifier)]
    #[must_use]
    pub fn subscription_identifier(&self) -> Option<u32> {
        self.subscription_identifier
    }

    #[wasm_bindgen(setter = subscriptionIdentifier)]
    pub fn set_subscription_identifier(&mut self, value: Option<u32>) {
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

#[wasm_bindgen(js_name = "WillMessage")]
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

#[wasm_bindgen(js_class = "WillMessage")]
impl WasmWillMessage {
    #[wasm_bindgen(constructor)]
    #[must_use]
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

    #[wasm_bindgen(getter = willDelayInterval)]
    #[must_use]
    pub fn will_delay_interval(&self) -> Option<u32> {
        self.will_delay_interval
    }

    #[wasm_bindgen(setter = willDelayInterval)]
    pub fn set_will_delay_interval(&mut self, value: Option<u32>) {
        self.will_delay_interval = value;
    }

    #[wasm_bindgen(getter = messageExpiryInterval)]
    #[must_use]
    pub fn message_expiry_interval(&self) -> Option<u32> {
        self.message_expiry_interval
    }

    #[wasm_bindgen(setter = messageExpiryInterval)]
    pub fn set_message_expiry_interval(&mut self, value: Option<u32>) {
        self.message_expiry_interval = value;
    }

    #[wasm_bindgen(getter = contentType)]
    #[must_use]
    pub fn content_type(&self) -> Option<String> {
        self.content_type.clone()
    }

    #[wasm_bindgen(setter = contentType)]
    pub fn set_content_type(&mut self, value: Option<String>) {
        self.content_type = value;
    }

    #[wasm_bindgen(getter = responseTopic)]
    #[must_use]
    pub fn response_topic(&self) -> Option<String> {
        self.response_topic.clone()
    }

    #[wasm_bindgen(setter = responseTopic)]
    pub fn set_response_topic(&mut self, value: Option<String>) {
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

#[wasm_bindgen(js_name = "MessageProperties")]
pub struct WasmMessageProperties {
    response_topic: Option<String>,
    correlation_data: Option<Vec<u8>>,
    content_type: Option<String>,
    payload_format_indicator: Option<bool>,
    message_expiry_interval: Option<u32>,
    subscription_identifiers: Vec<u32>,
    user_properties: Vec<(String, String)>,
}

#[wasm_bindgen(js_class = "MessageProperties")]
impl WasmMessageProperties {
    #[wasm_bindgen(getter = responseTopic)]
    #[must_use]
    pub fn response_topic(&self) -> Option<String> {
        self.response_topic.clone()
    }

    #[wasm_bindgen(getter = correlationData)]
    #[must_use]
    pub fn correlation_data(&self) -> Option<Vec<u8>> {
        self.correlation_data.clone()
    }

    #[wasm_bindgen(getter = contentType)]
    #[must_use]
    pub fn content_type(&self) -> Option<String> {
        self.content_type.clone()
    }

    #[wasm_bindgen(getter = payloadFormatIndicator)]
    #[must_use]
    pub fn payload_format_indicator(&self) -> Option<bool> {
        self.payload_format_indicator
    }

    #[wasm_bindgen(getter = messageExpiryInterval)]
    #[must_use]
    pub fn message_expiry_interval(&self) -> Option<u32> {
        self.message_expiry_interval
    }

    #[wasm_bindgen(getter = subscriptionIdentifiers)]
    #[must_use]
    pub fn subscription_identifiers(&self) -> Vec<u32> {
        self.subscription_identifiers.clone()
    }

    #[must_use]
    #[wasm_bindgen(js_name = getUserProperties)]
    pub fn get_user_properties(&self) -> js_sys::Array {
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
