use crate::error::{MqttError, Result};
use crate::transport::packet_io::encode_packet_to_buffer;
use crate::transport::quic::StreamStrategy;
use bytes::BytesMut;
use mqtt5_protocol::packet::Packet;
use quinn::{Connection, SendStream};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct QuicStreamManager {
    connection: Arc<Connection>,
    strategy: StreamStrategy,
    topic_streams: Arc<Mutex<HashMap<String, SendStream>>>,
    max_cached_streams: usize,
}

impl QuicStreamManager {
    pub fn new(connection: Arc<Connection>, strategy: StreamStrategy) -> Self {
        Self {
            connection,
            strategy,
            topic_streams: Arc::new(Mutex::new(HashMap::new())),
            max_cached_streams: 100,
        }
    }

    #[must_use]
    pub fn with_max_cached_streams(mut self, max: usize) -> Self {
        self.max_cached_streams = max;
        self
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

    async fn get_or_create_topic_stream(&self, topic: &str) -> Result<SendStream> {
        let mut streams = self.topic_streams.lock().await;

        if let Some(stream) = streams.remove(topic) {
            tracing::trace!(topic = %topic, "Reusing existing stream for topic");
            return Ok(stream);
        }

        if streams.len() >= self.max_cached_streams {
            tracing::debug!(
                "Stream cache full ({}/{}), clearing oldest entry",
                streams.len(),
                self.max_cached_streams
            );
            if let Some(key) = streams.keys().next().cloned() {
                streams.remove(&key);
            }
        }

        tracing::debug!(topic = %topic, "Opening new stream for topic");
        let (send, _recv) = self.connection.open_bi().await.map_err(|e| {
            MqttError::ConnectionError(format!("Failed to open QUIC stream for topic: {e}"))
        })?;

        Ok(send)
    }

    pub async fn send_on_topic_stream(&self, topic: String, packet: Packet) -> Result<()> {
        let mut stream = self.get_or_create_topic_stream(&topic).await?;

        let mut buf = BytesMut::with_capacity(1024);
        encode_packet_to_buffer(&packet, &mut buf)?;

        stream
            .write_all(&buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;

        tracing::debug!(topic = %topic, "Sent packet on topic-specific stream");

        self.topic_streams.lock().await.insert(topic, stream);

        Ok(())
    }

    pub async fn close_all_streams(&self) {
        let mut streams = self.topic_streams.lock().await;
        for (topic, mut stream) in streams.drain() {
            let _ = stream.finish();
            tracing::trace!(topic = %topic, "Closed topic stream");
        }
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
