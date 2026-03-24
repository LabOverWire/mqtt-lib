use crate::time::Duration;

// [MQoQ§5] Multi-stream modes
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamStrategy {
    #[default]
    ControlOnly,
    DataPerPublish,
    DataPerTopic,
    #[deprecated(note = "architecturally identical to DataPerTopic; use DataPerTopic instead")]
    DataPerSubscription,
}

#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct ClientTransportConfig {
    pub insecure_tls: bool,
    pub stream_strategy: StreamStrategy,
    pub flow_headers: bool,
    pub flow_expire: Duration,
    pub max_streams: Option<usize>,
    pub datagrams: bool,
    pub connect_timeout: Duration,
    pub enable_early_data: bool,
}

impl Default for ClientTransportConfig {
    fn default() -> Self {
        Self {
            insecure_tls: false,
            stream_strategy: StreamStrategy::default(),
            flow_headers: false,
            flow_expire: Duration::from_secs(300),
            max_streams: None,
            datagrams: false,
            connect_timeout: Duration::from_secs(30),
            enable_early_data: false,
        }
    }
}
