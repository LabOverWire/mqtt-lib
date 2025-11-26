use crate::error::{MqttError, Result};
use crate::transport::packet_io::encode_packet_to_buffer;
use crate::transport::quic::StreamStrategy;
use bytes::BytesMut;
use mqtt5_protocol::packet::Packet;
use quinn::{Connection, SendStream};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

struct StreamInfo {
    stream: SendStream,
    last_used: Instant,
}

const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

pub struct QuicStreamManager {
    connection: Arc<Connection>,
    strategy: StreamStrategy,
    topic_streams: Arc<Mutex<HashMap<String, StreamInfo>>>,
    max_cached_streams: usize,
    stream_idle_timeout: Duration,
}

impl QuicStreamManager {
    pub fn new(connection: Arc<Connection>, strategy: StreamStrategy) -> Self {
        Self {
            connection,
            strategy,
            topic_streams: Arc::new(Mutex::new(HashMap::new())),
            max_cached_streams: 100,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
        }
    }

    #[must_use]
    pub fn with_max_cached_streams(mut self, max: usize) -> Self {
        self.max_cached_streams = max;
        self
    }

    #[must_use]
    pub fn with_stream_idle_timeout(mut self, timeout: Duration) -> Self {
        self.stream_idle_timeout = timeout;
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
        let now = Instant::now();

        let idle_topics: Vec<String> = streams
            .iter()
            .filter(|(_, info)| now.duration_since(info.last_used) > self.stream_idle_timeout)
            .map(|(topic, _)| topic.clone())
            .collect();

        for idle_topic in &idle_topics {
            if let Some(mut info) = streams.remove(idle_topic) {
                let _ = info.stream.finish();
                tracing::debug!(topic = %idle_topic, "Closed idle stream");
            }
        }

        if let Some(info) = streams.remove(topic) {
            tracing::trace!(topic = %topic, "Reusing existing stream for topic");
            return Ok(info.stream);
        }

        if streams.len() >= self.max_cached_streams {
            let oldest = streams
                .iter()
                .min_by_key(|(_, info)| info.last_used)
                .map(|(k, _)| k.clone());

            if let Some(oldest_topic) = oldest {
                if let Some(mut info) = streams.remove(&oldest_topic) {
                    let _ = info.stream.finish();
                    tracing::debug!(
                        topic = %oldest_topic,
                        "Evicted oldest stream from cache (LRU)"
                    );
                }
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

        self.topic_streams.lock().await.insert(
            topic,
            StreamInfo {
                stream,
                last_used: Instant::now(),
            },
        );

        Ok(())
    }

    pub async fn send_on_subscription_stream(&self, topic: String, packet: Packet) -> Result<()> {
        self.send_on_topic_stream(topic, packet).await
    }

    pub async fn close_all_streams(&self) {
        let mut streams = self.topic_streams.lock().await;
        for (topic, mut info) in streams.drain() {
            let _ = info.stream.finish();
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
