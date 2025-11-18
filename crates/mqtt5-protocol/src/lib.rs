#![warn(clippy::pedantic)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unused_self)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::writeln_empty_string)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::if_not_else)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::doc_markdown)]
#![allow(dead_code)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::single_component_path_imports)]

pub mod constants;
pub mod encoding;
pub mod error;
pub mod flags;
pub mod packet;
pub mod packet_id;
pub mod protocol;
pub mod qos2;
pub mod time;
pub mod topic_matching;
pub mod types;
pub mod validation;

pub use error::{MqttError, Result};
pub use flags::{ConnAckFlags, ConnectFlags, PublishFlags};
pub use packet::{FixedHeader, Packet, PacketType};
pub use protocol::v5::properties::{Properties, PropertyId, PropertyValue, PropertyValueType};
pub use protocol::v5::reason_codes::ReasonCode;
pub use types::{
    ConnectOptions, ConnectProperties, ConnectResult, Message, MessageProperties, PublishOptions,
    PublishProperties, PublishResult, QoS, RetainHandling, SubscribeOptions, WillMessage,
    WillProperties,
};
pub use validation::{
    is_valid_client_id, is_valid_topic_filter, is_valid_topic_name, topic_matches_filter,
    validate_client_id, validate_topic_filter, validate_topic_name, RestrictiveValidator,
    StandardValidator, TopicValidator,
};
