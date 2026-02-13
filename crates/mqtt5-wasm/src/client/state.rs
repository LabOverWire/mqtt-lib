use crate::config::WasmConnectOptions;
use crate::transport::WasmWriter;
use mqtt5_protocol::connection::ReconnectConfig;
use mqtt5_protocol::packet_id::PacketIdGenerator;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

#[cfg(feature = "codec")]
use crate::codec::WasmCodecRegistry;

pub struct ClientState {
    pub client_id: String,
    pub writer: Option<Rc<RefCell<WasmWriter>>>,
    pub packet_id: PacketIdGenerator,
    pub connected: bool,
    pub protocol_version: u8,
    pub subscriptions: HashMap<String, js_sys::Function>,
    pub rust_subscriptions: HashMap<String, super::RustCallback>,
    pub pending_subacks: HashMap<u16, js_sys::Function>,
    pub pending_pubacks: HashMap<u16, js_sys::Function>,
    pub pending_pubcomps: HashMap<u16, (js_sys::Function, f64)>,
    pub pending_pubrecs: HashMap<u16, f64>,
    pub received_qos2: HashMap<u16, f64>,
    pub keep_alive: u16,
    pub last_ping_sent: Option<f64>,
    pub last_pong_received: Option<f64>,
    pub on_connect: Option<js_sys::Function>,
    pub on_disconnect: Option<js_sys::Function>,
    pub on_error: Option<js_sys::Function>,
    pub on_auth_challenge: Option<js_sys::Function>,
    pub on_reconnecting: Option<js_sys::Function>,
    pub on_reconnect_failed: Option<js_sys::Function>,
    pub auth_method: Option<String>,
    pub reconnect_config: ReconnectConfig,
    pub reconnect_attempt: u32,
    pub reconnecting: bool,
    pub last_url: Option<String>,
    pub last_options: Option<StoredConnectOptions>,
    pub user_initiated_disconnect: bool,
    pub connection_generation: u32,
    pub current_broker_index: usize,
    #[cfg(feature = "codec")]
    pub codec_registry: Option<Rc<WasmCodecRegistry>>,
}

impl ClientState {
    pub fn new(client_id: String) -> Self {
        Self {
            client_id,
            writer: None,
            packet_id: PacketIdGenerator::new(),
            connected: false,
            protocol_version: 5,
            subscriptions: HashMap::new(),
            rust_subscriptions: HashMap::new(),
            pending_subacks: HashMap::new(),
            pending_pubacks: HashMap::new(),
            pending_pubcomps: HashMap::new(),
            pending_pubrecs: HashMap::new(),
            received_qos2: HashMap::new(),
            keep_alive: 60,
            last_ping_sent: None,
            last_pong_received: None,
            on_connect: None,
            on_disconnect: None,
            on_error: None,
            on_auth_challenge: None,
            on_reconnecting: None,
            on_reconnect_failed: None,
            auth_method: None,
            reconnect_config: ReconnectConfig::disabled(),
            reconnect_attempt: 0,
            reconnecting: false,
            last_url: None,
            last_options: None,
            user_initiated_disconnect: false,
            connection_generation: 0,
            current_broker_index: 0,
            #[cfg(feature = "codec")]
            codec_registry: None,
        }
    }
}

#[derive(Clone)]
pub struct StoredConnectOptions {
    pub keep_alive: u16,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>,
    pub session_expiry_interval: Option<u32>,
    pub receive_maximum: Option<u16>,
    pub maximum_packet_size: Option<u32>,
    pub topic_alias_maximum: Option<u16>,
    pub request_response_information: Option<bool>,
    pub request_problem_information: Option<bool>,
    pub authentication_method: Option<String>,
    pub authentication_data: Option<Vec<u8>>,
    pub user_properties: Vec<(String, String)>,
    pub protocol_version: u8,
    pub backup_urls: Vec<String>,
    #[cfg(feature = "codec")]
    pub codec_registry: Option<Rc<WasmCodecRegistry>>,
}

impl From<&WasmConnectOptions> for StoredConnectOptions {
    fn from(opts: &WasmConnectOptions) -> Self {
        Self {
            keep_alive: opts.keep_alive,
            username: opts.username.clone(),
            password: opts.password.clone(),
            session_expiry_interval: opts.session_expiry_interval,
            receive_maximum: opts.receive_maximum,
            maximum_packet_size: opts.maximum_packet_size,
            topic_alias_maximum: opts.topic_alias_maximum,
            request_response_information: opts.request_response_information,
            request_problem_information: opts.request_problem_information,
            authentication_method: opts.authentication_method.clone(),
            authentication_data: opts.authentication_data.clone(),
            user_properties: opts.user_properties.clone(),
            protocol_version: opts.protocol_version,
            backup_urls: opts.backup_urls.clone(),
            #[cfg(feature = "codec")]
            codec_registry: opts.codec_registry.clone(),
        }
    }
}
