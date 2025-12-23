use bytes::Bytes;
use mqtt5_protocol::packet::suback::SubAckReasonCode as ProtocolSubAckReasonCode;
use mqtt5_protocol::types::ReasonCode;
use mqtt5_protocol::QoS;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ClientConnectEvent {
    pub client_id: Arc<str>,
    pub clean_start: bool,
    pub session_expiry_interval: u32,
    pub will_topic: Option<Arc<str>>,
    pub will_payload: Option<Bytes>,
    pub will_qos: Option<QoS>,
    pub will_retain: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAckReasonCode {
    GrantedQoS0,
    GrantedQoS1,
    GrantedQoS2,
    UnspecifiedError,
    ImplementationSpecificError,
    NotAuthorized,
    TopicFilterInvalid,
    PacketIdentifierInUse,
    QuotaExceeded,
    SharedSubscriptionsNotSupported,
    SubscriptionIdentifiersNotSupported,
    WildcardSubscriptionsNotSupported,
}

impl From<ProtocolSubAckReasonCode> for SubAckReasonCode {
    fn from(code: ProtocolSubAckReasonCode) -> Self {
        match code {
            ProtocolSubAckReasonCode::GrantedQoS0 => Self::GrantedQoS0,
            ProtocolSubAckReasonCode::GrantedQoS1 => Self::GrantedQoS1,
            ProtocolSubAckReasonCode::GrantedQoS2 => Self::GrantedQoS2,
            ProtocolSubAckReasonCode::UnspecifiedError => Self::UnspecifiedError,
            ProtocolSubAckReasonCode::ImplementationSpecificError => {
                Self::ImplementationSpecificError
            }
            ProtocolSubAckReasonCode::NotAuthorized => Self::NotAuthorized,
            ProtocolSubAckReasonCode::TopicFilterInvalid => Self::TopicFilterInvalid,
            ProtocolSubAckReasonCode::PacketIdentifierInUse => Self::PacketIdentifierInUse,
            ProtocolSubAckReasonCode::QuotaExceeded => Self::QuotaExceeded,
            ProtocolSubAckReasonCode::SharedSubscriptionsNotSupported => {
                Self::SharedSubscriptionsNotSupported
            }
            ProtocolSubAckReasonCode::SubscriptionIdentifiersNotSupported => {
                Self::SubscriptionIdentifiersNotSupported
            }
            ProtocolSubAckReasonCode::WildcardSubscriptionsNotSupported => {
                Self::WildcardSubscriptionsNotSupported
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct SubscriptionInfo {
    pub topic_filter: Arc<str>,
    pub qos: QoS,
    pub result: SubAckReasonCode,
}

#[derive(Debug, Clone)]
pub struct ClientSubscribeEvent {
    pub client_id: Arc<str>,
    pub subscriptions: Vec<SubscriptionInfo>,
}

#[derive(Debug, Clone)]
pub struct ClientUnsubscribeEvent {
    pub client_id: Arc<str>,
    pub topic_filters: Vec<Arc<str>>,
}

#[derive(Debug, Clone)]
pub struct ClientPublishEvent {
    pub client_id: Arc<str>,
    pub topic: Arc<str>,
    pub payload: Bytes,
    pub qos: QoS,
    pub retain: bool,
    pub packet_id: Option<u16>,
}

#[derive(Debug, Clone)]
pub struct ClientDisconnectEvent {
    pub client_id: Arc<str>,
    pub reason: ReasonCode,
    pub unexpected: bool,
}

#[derive(Debug, Clone)]
pub struct RetainedSetEvent {
    pub topic: Arc<str>,
    pub payload: Bytes,
    pub qos: QoS,
    pub cleared: bool,
}

#[derive(Debug, Clone)]
pub struct MessageDeliveredEvent {
    pub client_id: Arc<str>,
    pub packet_id: u16,
    pub qos: QoS,
}

pub trait BrokerEventHandler: Send + Sync {
    fn on_client_connect<'a>(
        &'a self,
        _event: ClientConnectEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn on_client_subscribe<'a>(
        &'a self,
        _event: ClientSubscribeEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn on_client_unsubscribe<'a>(
        &'a self,
        _event: ClientUnsubscribeEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn on_client_publish<'a>(
        &'a self,
        _event: ClientPublishEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn on_client_disconnect<'a>(
        &'a self,
        _event: ClientDisconnectEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn on_retained_set<'a>(
        &'a self,
        _event: RetainedSetEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }

    fn on_message_delivered<'a>(
        &'a self,
        _event: MessageDeliveredEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
    }
}
