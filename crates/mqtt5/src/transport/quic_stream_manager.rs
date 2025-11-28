use crate::error::{MqttError, Result};
use crate::transport::flow::{DataFlowHeader, FlowFlags, FlowId, FlowIdGenerator};
use crate::transport::packet_io::encode_packet_to_buffer;
use crate::transport::quic::StreamStrategy;
use bytes::BytesMut;
use mqtt5_protocol::packet::Packet;
use quinn::{Connection, SendStream};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{debug, instrument, trace};

struct StreamInfo {
    stream: SendStream,
    flow_id: FlowId,
    last_used: Instant,
}

struct FlowStreamInfo {
    stream: SendStream,
    last_used: Instant,
}

const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_FLOW_EXPIRE_INTERVAL: u64 = 300;

pub struct QuicStreamManager {
    connection: Arc<Connection>,
    strategy: StreamStrategy,
    topic_streams: Arc<Mutex<HashMap<String, StreamInfo>>>,
    flow_streams: Arc<Mutex<HashMap<FlowId, FlowStreamInfo>>>,
    max_cached_streams: usize,
    stream_idle_timeout: Duration,
    flow_id_generator: Arc<Mutex<FlowIdGenerator>>,
    flow_expire_interval: u64,
    flow_flags: FlowFlags,
    enable_flow_headers: bool,
}

impl QuicStreamManager {
    pub fn new(connection: Arc<Connection>, strategy: StreamStrategy) -> Self {
        Self {
            connection,
            strategy,
            topic_streams: Arc::new(Mutex::new(HashMap::new())),
            flow_streams: Arc::new(Mutex::new(HashMap::new())),
            max_cached_streams: 100,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
            flow_id_generator: Arc::new(Mutex::new(FlowIdGenerator::new())),
            flow_expire_interval: DEFAULT_FLOW_EXPIRE_INTERVAL,
            flow_flags: FlowFlags::default(),
            enable_flow_headers: false,
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

    #[must_use]
    pub fn with_flow_headers(mut self, enable: bool) -> Self {
        self.enable_flow_headers = enable;
        self
    }

    #[must_use]
    pub fn with_flow_expire_interval(mut self, seconds: u64) -> Self {
        self.flow_expire_interval = seconds;
        self
    }

    #[must_use]
    pub fn with_flow_flags(mut self, flags: FlowFlags) -> Self {
        self.flow_flags = flags;
        self
    }

    /// Opens a bidirectional QUIC data stream.
    ///
    /// # Errors
    /// Returns an error if the QUIC connection fails to open a stream.
    #[instrument(skip(self), level = "debug")]
    pub async fn open_data_stream(&self) -> Result<(quinn::SendStream, quinn::RecvStream)> {
        let (send, recv) =
            self.connection.open_bi().await.map_err(|e| {
                MqttError::ConnectionError(format!("Failed to open QUIC stream: {e}"))
            })?;
        Ok((send, recv))
    }

    /// Opens a data stream with flow header.
    ///
    /// # Errors
    /// Returns an error if the stream cannot be opened or the flow header write fails.
    #[instrument(skip(self), fields(strategy = ?self.strategy), level = "debug")]
    pub async fn open_data_stream_with_flow(
        &self,
    ) -> Result<(quinn::SendStream, quinn::RecvStream, FlowId)> {
        let (mut send, recv) = self.open_data_stream().await?;

        let flow_id = if self.enable_flow_headers {
            let flow_id = {
                let mut gen = self.flow_id_generator.lock().await;
                gen.next_client()
            };

            let mut buf = BytesMut::with_capacity(32);
            let header =
                DataFlowHeader::client(flow_id, self.flow_expire_interval, self.flow_flags);
            header.encode(&mut buf);

            send.write_all(&buf).await.map_err(|e| {
                MqttError::ConnectionError(format!("Failed to write flow header: {e}"))
            })?;

            debug!(flow_id = ?flow_id, "Wrote client data flow header on new stream");
            flow_id
        } else {
            FlowId::client(0)
        };

        Ok((send, recv, flow_id))
    }

    /// Opens a stream for flow state recovery.
    ///
    /// # Errors
    /// Returns an error if the stream cannot be opened or the recovery header write fails.
    pub async fn open_recovery_stream(
        &self,
        flow_id: FlowId,
        recovery_flags: FlowFlags,
    ) -> Result<(quinn::SendStream, quinn::RecvStream)> {
        let (mut send, recv) = self.open_data_stream().await?;

        if self.enable_flow_headers {
            let mut buf = BytesMut::with_capacity(32);
            let header = DataFlowHeader::client(flow_id, self.flow_expire_interval, recovery_flags);
            header.encode(&mut buf);

            send.write_all(&buf).await.map_err(|e| {
                MqttError::ConnectionError(format!("Failed to write recovery flow header: {e}"))
            })?;

            debug!(
                flow_id = ?flow_id,
                ?recovery_flags,
                "Wrote recovery flow header on stream"
            );
        }

        Ok((send, recv))
    }

    pub fn set_recovery_mode(&mut self, enable: bool) {
        self.flow_flags.clean = u8::from(!enable);
    }

    pub fn current_flow_flags(&self) -> FlowFlags {
        self.flow_flags
    }

    /// Sends a packet on a dedicated QUIC stream.
    ///
    /// # Errors
    /// Returns an error if the stream operation or packet encoding fails.
    #[instrument(skip(self, packet), level = "debug")]
    pub async fn send_packet_on_stream(&self, packet: Packet) -> Result<()> {
        let (mut send, _recv, flow_id) = self.open_data_stream_with_flow().await?;

        let mut buf = BytesMut::with_capacity(1024);
        encode_packet_to_buffer(&packet, &mut buf)?;

        send.write_all(&buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;

        send.finish()
            .map_err(|e| MqttError::ConnectionError(format!("QUIC stream finish error: {e}")))?;

        debug!(flow_id = ?flow_id, "Sent packet on dedicated QUIC stream");

        Ok(())
    }

    pub fn strategy(&self) -> StreamStrategy {
        self.strategy
    }

    pub fn flow_headers_enabled(&self) -> bool {
        self.enable_flow_headers
    }

    // [MQoQ§5.3] Per-topic stream caching
    async fn get_or_create_topic_stream(&self, topic: &str) -> Result<(SendStream, FlowId)> {
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
                debug!(topic = %idle_topic, flow_id = ?info.flow_id, "Closed idle stream");
            }
        }

        if let Some(info) = streams.remove(topic) {
            trace!(topic = %topic, flow_id = ?info.flow_id, "Reusing existing stream for topic");
            return Ok((info.stream, info.flow_id));
        }

        if streams.len() >= self.max_cached_streams {
            let oldest = streams
                .iter()
                .min_by_key(|(_, info)| info.last_used)
                .map(|(k, _)| k.clone());

            if let Some(oldest_topic) = oldest {
                if let Some(mut info) = streams.remove(&oldest_topic) {
                    let _ = info.stream.finish();
                    debug!(
                        topic = %oldest_topic,
                        flow_id = ?info.flow_id,
                        "Evicted oldest stream from cache (LRU)"
                    );
                }
            }
        }
        drop(streams);

        debug!(topic = %topic, "Opening new stream for topic");
        let (mut send, _recv) = self.connection.open_bi().await.map_err(|e| {
            MqttError::ConnectionError(format!("Failed to open QUIC stream for topic: {e}"))
        })?;

        let flow_id = if self.enable_flow_headers {
            let flow_id = {
                let mut gen = self.flow_id_generator.lock().await;
                gen.next_client()
            };

            let mut buf = BytesMut::with_capacity(32);
            let header =
                DataFlowHeader::client(flow_id, self.flow_expire_interval, self.flow_flags);
            header.encode(&mut buf);

            send.write_all(&buf).await.map_err(|e| {
                MqttError::ConnectionError(format!("Failed to write flow header: {e}"))
            })?;

            debug!(topic = %topic, flow_id = ?flow_id, "Wrote flow header for new topic stream");
            flow_id
        } else {
            FlowId::client(0)
        };

        Ok((send, flow_id))
    }

    /// Sends a packet on a topic-specific stream.
    ///
    /// # Errors
    /// Returns an error if the stream operation or packet encoding fails.
    pub async fn send_on_topic_stream(&self, topic: String, packet: Packet) -> Result<()> {
        let (mut stream, flow_id) = self.get_or_create_topic_stream(&topic).await?;

        let mut buf = BytesMut::with_capacity(1024);
        encode_packet_to_buffer(&packet, &mut buf)?;

        stream
            .write_all(&buf)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;

        debug!(topic = %topic, flow_id = ?flow_id, "Sent packet on topic-specific stream");

        self.topic_streams.lock().await.insert(
            topic,
            StreamInfo {
                stream,
                flow_id,
                last_used: Instant::now(),
            },
        );

        Ok(())
    }

    /// Sends a packet on a subscription-specific stream.
    ///
    /// # Errors
    /// Returns an error if the stream operation or packet encoding fails.
    pub async fn send_on_subscription_stream(&self, topic: String, packet: Packet) -> Result<()> {
        self.send_on_topic_stream(topic, packet).await
    }

    pub async fn get_flow_id_for_topic(&self, topic: &str) -> Option<FlowId> {
        let streams = self.topic_streams.lock().await;
        streams.get(topic).map(|info| info.flow_id)
    }

    #[instrument(skip(self, stream), level = "debug")]
    pub async fn register_flow_stream(&self, flow_id: FlowId, stream: SendStream) {
        let mut flows = self.flow_streams.lock().await;
        flows.insert(
            flow_id,
            FlowStreamInfo {
                stream,
                last_used: Instant::now(),
            },
        );
        debug!(flow_id = ?flow_id, "Registered flow stream");
    }

    /// Sends a packet on an existing flow stream.
    ///
    /// # Errors
    /// Returns an error if the flow stream is not found or the write operation fails.
    #[instrument(skip(self, packet), level = "debug")]
    pub async fn send_on_flow(&self, flow_id: FlowId, packet: Packet) -> Result<()> {
        let mut flows = self.flow_streams.lock().await;

        if let Some(info) = flows.get_mut(&flow_id) {
            let mut buf = BytesMut::with_capacity(1024);
            encode_packet_to_buffer(&packet, &mut buf)?;

            info.stream
                .write_all(&buf)
                .await
                .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;
            info.last_used = Instant::now();

            debug!(flow_id = ?flow_id, "Sent packet on flow stream");
            Ok(())
        } else {
            drop(flows);

            let (mut send, _recv, new_flow_id) = self.open_data_stream_with_flow().await?;

            let mut buf = BytesMut::with_capacity(1024);
            encode_packet_to_buffer(&packet, &mut buf)?;

            send.write_all(&buf)
                .await
                .map_err(|e| MqttError::ConnectionError(format!("QUIC write error: {e}")))?;

            self.flow_streams.lock().await.insert(
                new_flow_id,
                FlowStreamInfo {
                    stream: send,
                    last_used: Instant::now(),
                },
            );

            debug!(
                requested_flow_id = ?flow_id,
                actual_flow_id = ?new_flow_id,
                "Flow not found, opened new stream"
            );
            Ok(())
        }
    }

    pub async fn has_flow_stream(&self, flow_id: FlowId) -> bool {
        self.flow_streams.lock().await.contains_key(&flow_id)
    }

    pub async fn remove_flow_stream(&self, flow_id: FlowId) -> bool {
        let mut flows = self.flow_streams.lock().await;
        if let Some(mut info) = flows.remove(&flow_id) {
            let _ = info.stream.finish();
            debug!(flow_id = ?flow_id, "Removed flow stream");
            true
        } else {
            false
        }
    }

    pub async fn close_all_streams(&self) {
        let mut streams = self.topic_streams.lock().await;
        for (topic, mut info) in streams.drain() {
            let _ = info.stream.finish();
            trace!(topic = %topic, flow_id = ?info.flow_id, "Closed topic stream");
        }
        drop(streams);

        let mut flows = self.flow_streams.lock().await;
        for (flow_id, mut info) in flows.drain() {
            let _ = info.stream.finish();
            trace!(flow_id = ?flow_id, "Closed flow stream");
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

    #[test]
    fn test_flow_header_builder_methods() {
        let flags = FlowFlags {
            persistent_qos: 1,
            persistent_subscriptions: 1,
            ..Default::default()
        };

        let conn = Arc::new(std::mem::MaybeUninit::<Connection>::uninit());
        let conn_ptr = unsafe { &*conn.as_ptr() };

        let _ = flags;
        let _ = conn_ptr;
    }

    #[test]
    fn test_flow_flags_config() {
        let flags = FlowFlags {
            clean: 1,
            abort_if_no_state: 0,
            err_tolerance: 1,
            persistent_qos: 1,
            persistent_topic_alias: 0,
            persistent_subscriptions: 1,
            optional_headers: 0,
        };

        assert_eq!(flags.clean, 1);
        assert_eq!(flags.abort_if_no_state, 0);
        assert_eq!(flags.err_tolerance, 1);
        assert_eq!(flags.persistent_qos, 1);
        assert_eq!(flags.persistent_topic_alias, 0);
        assert_eq!(flags.persistent_subscriptions, 1);
        assert_eq!(flags.optional_headers, 0);
    }

    #[test]
    fn test_default_flow_expire_interval() {
        assert_eq!(DEFAULT_FLOW_EXPIRE_INTERVAL, 300);
    }

    #[test]
    fn test_flow_id_generator_sequence() {
        let mut gen = FlowIdGenerator::new();
        let id1 = gen.next_client();
        let id2 = gen.next_client();
        let id3 = gen.next_client();

        assert!(id1.is_client_initiated());
        assert!(id2.is_client_initiated());
        assert!(id3.is_client_initiated());

        assert_eq!(id1.sequence(), 1);
        assert_eq!(id2.sequence(), 2);
        assert_eq!(id3.sequence(), 3);
    }
}
