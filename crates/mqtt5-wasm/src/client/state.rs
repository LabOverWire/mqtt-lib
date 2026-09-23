use crate::config::WasmConnectOptions;
use crate::transport::WasmWriter;
use mqtt5_protocol::connection::ReconnectConfig;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet_id::PacketIdGenerator;
use mqtt5_protocol::protocol::v5::properties::{PropertyId, PropertyValue};
use mqtt5_protocol::session::TopicAliasManager;
use mqtt5_protocol::QoS;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

#[cfg(feature = "codec")]
use crate::codec::WasmCodecRegistry;

const DEFAULT_RECEIVE_MAXIMUM: u16 = u16::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerLimits {
    pub receive_maximum: u16,
    pub maximum_qos: QoS,
    pub retain_available: bool,
    pub maximum_packet_size: Option<u32>,
    pub topic_alias_maximum: u16,
    pub subscriptions: SubscriptionFeatures,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionFeatures {
    pub wildcards: bool,
    pub shared: bool,
    pub identifiers: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Absent,
    Held,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            receive_maximum: DEFAULT_RECEIVE_MAXIMUM,
            maximum_qos: QoS::ExactlyOnce,
            retain_available: true,
            maximum_packet_size: None,
            topic_alias_maximum: 0,
            subscriptions: SubscriptionFeatures {
                wildcards: true,
                shared: true,
                identifiers: true,
            },
        }
    }
}

impl ServerLimits {
    pub fn from_connack(connack: &ConnAckPacket) -> Self {
        let properties = &connack.properties;
        let available =
            |id: PropertyId| !matches!(properties.get(id), Some(PropertyValue::Byte(0)));
        Self {
            receive_maximum: connack
                .receive_maximum()
                .filter(|max| *max > 0)
                .unwrap_or(DEFAULT_RECEIVE_MAXIMUM),
            maximum_qos: match properties.get_maximum_qos() {
                Some(0) => QoS::AtMostOnce,
                Some(1) => QoS::AtLeastOnce,
                _ => QoS::ExactlyOnce,
            },
            retain_available: available(PropertyId::RetainAvailable),
            maximum_packet_size: connack.maximum_packet_size(),
            topic_alias_maximum: connack.topic_alias_maximum().unwrap_or(0),
            subscriptions: SubscriptionFeatures {
                wildcards: available(PropertyId::WildcardSubscriptionAvailable),
                shared: available(PropertyId::SharedSubscriptionAvailable),
                identifiers: available(PropertyId::SubscriptionIdentifierAvailable),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientLimits {
    pub receive_maximum: u16,
    pub maximum_packet_size: u32,
    pub topic_alias_maximum: u16,
    pub request_problem_information: bool,
}

impl Default for ClientLimits {
    fn default() -> Self {
        Self::from(&StoredConnectOptions::from(&WasmConnectOptions::default()))
    }
}

impl From<&StoredConnectOptions> for ClientLimits {
    fn from(options: &StoredConnectOptions) -> Self {
        Self {
            receive_maximum: options
                .receive_maximum
                .filter(|max| *max > 0)
                .unwrap_or(DEFAULT_RECEIVE_MAXIMUM),
            maximum_packet_size: options
                .maximum_packet_size
                .filter(|size| *size > 0)
                .unwrap_or(mqtt5_protocol::constants::limits::MAX_PACKET_SIZE),
            topic_alias_maximum: options.topic_alias_maximum.unwrap_or(0),
            request_problem_information: options.request_problem_information.unwrap_or(true),
        }
    }
}

pub struct OutboundFlight {
    pub sequence: u64,
    pub publish: PublishPacket,
    pub released: bool,
}

pub struct ClientState {
    pub client_id: String,
    pub writer: Option<Rc<RefCell<WasmWriter>>>,
    pub packet_id: PacketIdGenerator,
    pub connected: bool,
    pub protocol_version: u8,
    pub subscriptions: HashMap<String, js_sys::Function>,
    pub rust_subscriptions: HashMap<String, super::RustCallback>,
    pub pending_subacks: HashMap<u16, Option<js_sys::Function>>,
    pub pending_unsubacks: HashSet<u16>,
    pub pending_pubacks: HashMap<u16, js_sys::Function>,
    pub pending_pubcomps: HashMap<u16, (js_sys::Function, f64)>,
    pub outbound: HashMap<u16, OutboundFlight>,
    pub next_flight_sequence: u64,
    pub send_quota: u16,
    pub quota_waiters: VecDeque<js_sys::Function>,
    pub pending_resends: VecDeque<u16>,
    pub awaiting_pubrel: HashSet<u16>,
    pub server: ServerLimits,
    pub client_limits: ClientLimits,
    pub outbound_aliases: TopicAliasManager,
    pub inbound_aliases: TopicAliasManager,
    pub session: SessionState,
    pub session_expiry_interval: u32,
    pub keep_alive: u16,
    pub last_ping_sent: Option<f64>,
    pub last_pong_received: Option<f64>,
    pub on_connect: Option<js_sys::Function>,
    pub on_disconnect: Option<js_sys::Function>,
    pub on_error: Option<js_sys::Function>,
    pub on_auth_challenge: Option<js_sys::Function>,
    pub on_reconnecting: Option<js_sys::Function>,
    pub on_reconnect_failed: Option<js_sys::Function>,
    pub on_connectivity_change: Option<js_sys::Function>,
    pub online_listener_fn: Option<js_sys::Function>,
    pub offline_listener_fn: Option<js_sys::Function>,
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
            pending_unsubacks: HashSet::new(),
            pending_pubacks: HashMap::new(),
            pending_pubcomps: HashMap::new(),
            outbound: HashMap::new(),
            next_flight_sequence: 0,
            send_quota: DEFAULT_RECEIVE_MAXIMUM,
            quota_waiters: VecDeque::new(),
            pending_resends: VecDeque::new(),
            awaiting_pubrel: HashSet::new(),
            server: ServerLimits::default(),
            client_limits: ClientLimits::default(),
            outbound_aliases: TopicAliasManager::new(0),
            inbound_aliases: TopicAliasManager::new(0),
            session: SessionState::Absent,
            session_expiry_interval: 0,
            keep_alive: 60,
            last_ping_sent: None,
            last_pong_received: None,
            on_connect: None,
            on_disconnect: None,
            on_error: None,
            on_auth_challenge: None,
            on_reconnecting: None,
            on_reconnect_failed: None,
            on_connectivity_change: None,
            online_listener_fn: None,
            offline_listener_fn: None,
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

    pub fn packet_id_in_use(&self, packet_id: u16) -> bool {
        self.outbound.contains_key(&packet_id)
            || self.pending_subacks.contains_key(&packet_id)
            || self.pending_unsubacks.contains(&packet_id)
    }

    pub fn allocate_packet_id(&self) -> Option<u16> {
        (0..=u16::MAX).find_map(|_| {
            let packet_id = self.packet_id.next();
            (!self.packet_id_in_use(packet_id)).then_some(packet_id)
        })
    }

    pub fn record_flight(&mut self, packet_id: u16, publish: PublishPacket) {
        let sequence = self.next_flight_sequence;
        self.next_flight_sequence = sequence.wrapping_add(1);
        self.outbound.insert(
            packet_id,
            OutboundFlight {
                sequence,
                publish,
                released: false,
            },
        );
    }

    pub fn discard_session(&mut self) -> Vec<js_sys::Function> {
        self.outbound.clear();
        self.pending_resends.clear();
        self.awaiting_pubrel.clear();
        self.session = SessionState::Absent;
        self.pending_pubacks
            .drain()
            .map(|(_, callback)| callback)
            .chain(
                self.pending_pubcomps
                    .drain()
                    .map(|(_, (callback, _))| callback),
            )
            .collect()
    }

    pub fn apply_connect_options(&mut self, options: &StoredConnectOptions) {
        self.keep_alive = options.keep_alive;
        self.protocol_version = options.protocol_version;
        self.session_expiry_interval = options.session_expiry_interval.unwrap_or(0);
        self.client_limits = ClientLimits::from(options);
        self.auth_method.clone_from(&options.authentication_method);
        #[cfg(feature = "codec")]
        {
            self.codec_registry.clone_from(&options.codec_registry);
        }
    }

    pub fn apply_connack(&mut self, connack: &ConnAckPacket) {
        self.server = ServerLimits::from_connack(connack);
        self.send_quota = self.server.receive_maximum;
        self.outbound_aliases = TopicAliasManager::new(self.server.topic_alias_maximum);
        self.inbound_aliases = TopicAliasManager::new(self.client_limits.topic_alias_maximum);
        self.pending_resends.clear();
        if let Some(keep_alive) = connack.properties.get_server_keep_alive() {
            self.keep_alive = keep_alive;
        }
        if let Some(PropertyValue::Utf8String(assigned)) =
            connack.properties.get(PropertyId::AssignedClientIdentifier)
        {
            self.client_id.clone_from(assigned);
        }
        self.last_ping_sent = None;
        self.last_pong_received = None;
    }
}

#[derive(Clone)]
pub struct StoredConnectOptions {
    pub keep_alive: u16,
    pub resume_existing_session: bool,
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
            resume_existing_session: opts.resume_existing_session,
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
