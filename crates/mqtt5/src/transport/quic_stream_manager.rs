use crate::error::Result;
use crate::transport::quic::StreamStrategy;
use quinn::Connection;
use std::sync::Arc;

pub struct QuicStreamManager {
    connection: Arc<Connection>,
    strategy: StreamStrategy,
}

impl QuicStreamManager {
    pub fn new(connection: Arc<Connection>, strategy: StreamStrategy) -> Self {
        Self {
            connection,
            strategy,
        }
    }

    pub fn get_control_stream(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_manager_creation() {
        let strategy = StreamStrategy::ControlOnly;
        assert_eq!(strategy, StreamStrategy::default());
    }

    #[test]
    fn test_stream_strategy_variants() {
        assert_eq!(StreamStrategy::ControlOnly, StreamStrategy::default());
        assert_ne!(StreamStrategy::DataPerPublish, StreamStrategy::ControlOnly);
        assert_ne!(StreamStrategy::DataPerTopic, StreamStrategy::ControlOnly);
        assert_ne!(StreamStrategy::DataPerSubscription, StreamStrategy::ControlOnly);
    }
}
