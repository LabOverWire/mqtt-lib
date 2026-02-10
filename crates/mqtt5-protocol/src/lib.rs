#![cfg_attr(not(feature = "std"), no_std)]
#![warn(clippy::pedantic)]

#[cfg(not(feature = "std"))]
extern crate alloc;

mod prelude;

pub mod bridge;
pub mod connection;
pub mod constants;
pub mod encoding;
pub mod error;
pub mod error_classification;
pub mod flags;
pub mod keepalive;
pub mod numeric;
pub mod packet;
pub mod packet_id;
pub mod protocol;
pub mod qos2;
pub mod session;
pub mod time;
pub mod topic_matching;
pub mod transport;
pub mod types;
pub mod validation;

pub use error::{MqttError, Result};
pub use flags::{ConnAckFlags, ConnectFlags, PublishFlags};
pub use packet::{FixedHeader, Packet, PacketType};
pub use protocol::v5::properties::{Properties, PropertyId, PropertyValue, PropertyValueType};
pub use protocol::v5::reason_codes::ReasonCode;
pub use transport::Transport;
pub use types::{
    ConnectOptions, ConnectProperties, ConnectResult, Message, MessageProperties, ProtocolVersion,
    PublishOptions, PublishProperties, PublishResult, QoS, RetainHandling, SubscribeOptions,
    WillMessage, WillProperties,
};
pub use validation::{
    is_path_safe_client_id, is_valid_client_id, is_valid_topic_filter, is_valid_topic_name,
    parse_shared_subscription, strip_shared_subscription_prefix, topic_matches_filter,
    validate_client_id, validate_topic_filter, validate_topic_name, RestrictiveValidator,
    StandardValidator, TopicValidator,
};

pub use session::{
    ExpiringMessage, FlowControlConfig, FlowControlStats, LimitsConfig, LimitsManager,
    MessageQueue, QueueResult, QueueStats, QueuedMessage, Subscription, SubscriptionManager,
    TopicAliasManager,
};

pub use connection::{
    ConnectionEvent, ConnectionInfo, ConnectionState, ConnectionStateMachine, DisconnectReason,
    ReconnectConfig,
};

pub use keepalive::{calculate_ping_interval, is_keepalive_timeout, KeepaliveConfig};

pub use error_classification::RecoverableError;

pub use numeric::{
    i32_to_u32_saturating, u128_to_f64_saturating, u128_to_u32_saturating, u128_to_u64_saturating,
    u64_to_f64_saturating, u64_to_u16_saturating, u64_to_u32_saturating, usize_to_f64_saturating,
    usize_to_u16_saturating, usize_to_u32_saturating, F64_MAX_SAFE_INTEGER,
};

pub use bridge::{
    evaluate_forwarding, BridgeDirection, BridgeStats, ForwardingDecision, TopicMappingCore,
};
