use crate::error::{MqttError, Result};
use crate::transport::packet_io::encode_packet_to_buffer;
use crate::transport::quic::StreamStrategy;
use bytes::BytesMut;
use mqtt5_protocol::packet::Packet;
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

    pub async fn open_data_stream(&self) -> Result<(quinn::SendStream, quinn::RecvStream)> {
        self.connection
            .open_bi()
            .await
            .map_err(|e| MqttError::ConnectionError(format!("Failed to open QUIC stream: {e}")))
    }

    pub async fn send_packet_on_stream(&self, packet: Packet) -> Result<()> {
        let (mut send, _recv) = self.open_data_stream().await?;

        let mut buf = BytesMut::with_capacity(1024);
        encode_packet_to_buffer(&packet, &mut buf)?;

        send.write_all(&buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;

        send.finish()
            .map_err(|e| MqttError::ConnectionError(format!("QUIC stream finish error: {e}")))?;

        tracing::debug!("Sent packet on dedicated QUIC stream");

        Ok(())
    }

    pub fn strategy(&self) -> StreamStrategy {
        self.strategy
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
        assert_ne!(
            StreamStrategy::DataPerSubscription,
            StreamStrategy::ControlOnly
        );
    }
}
