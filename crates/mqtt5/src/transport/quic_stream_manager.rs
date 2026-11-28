use crate::client::{Flush, RoomReservation, UnifiedWriter};
use crate::error::{MqttError, Result};
use crate::transport::client_config::StreamStrategy;
use crate::transport::flow::{DataFlowHeader, FlowFlags, FlowId, FlowIdGenerator};
use bytes::BytesMut;
use mqtt5_protocol::packet::Packet;
use parking_lot::Mutex;
use quinn::{Connection, SendStream};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Notify, Semaphore};
use tracing::{debug, instrument, trace, warn};

type SharedWriter = Arc<UnifiedWriter>;

struct StreamInfo {
    stream: SharedWriter,
    flow_id: FlowId,
    last_used: Mutex<Instant>,
    reserved: AtomicUsize,
}

impl StreamInfo {
    fn evictable(&self) -> bool {
        self.reserved.load(Ordering::Acquire) == 0 && self.stream.try_finish_idle()
    }
}

struct FlowStreamInfo {
    stream: SharedWriter,
    last_used: Instant,
}

const DEFAULT_STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const DEFAULT_MAX_CACHED_STREAMS: usize = 100;
const DEFAULT_FLOW_EXPIRE_INTERVAL: u64 = 300;

pub struct QuicStreamManager {
    connection: Arc<Connection>,
    strategy: StreamStrategy,
    topic_streams: Mutex<HashMap<String, Arc<StreamInfo>>>,
    flow_streams: Mutex<HashMap<FlowId, FlowStreamInfo>>,
    max_cached_streams: usize,
    stream_idle_timeout: Duration,
    drained: Arc<Notify>,
    dedicated_slots: Arc<Semaphore>,
    closed: AtomicBool,
    flow_id_generator: Mutex<FlowIdGenerator>,
    flow_expire_interval: u64,
    flow_flags: FlowFlags,
    enable_flow_headers: bool,
}

impl QuicStreamManager {
    #[must_use]
    pub fn new(connection: Arc<Connection>, strategy: StreamStrategy) -> Self {
        Self {
            connection,
            strategy,
            topic_streams: Mutex::new(HashMap::new()),
            flow_streams: Mutex::new(HashMap::new()),
            max_cached_streams: DEFAULT_MAX_CACHED_STREAMS,
            stream_idle_timeout: DEFAULT_STREAM_IDLE_TIMEOUT,
            drained: Arc::new(Notify::new()),
            dedicated_slots: Arc::new(Semaphore::new(DEFAULT_MAX_CACHED_STREAMS)),
            closed: AtomicBool::new(false),
            flow_id_generator: Mutex::new(FlowIdGenerator::new()),
            flow_expire_interval: DEFAULT_FLOW_EXPIRE_INTERVAL,
            flow_flags: FlowFlags::default(),
            enable_flow_headers: false,
        }
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

    /// # Errors
    /// Returns an error if the QUIC connection fails to open a stream.
    #[instrument(skip(self), level = "debug")]
    pub async fn open_data_stream(&self) -> Result<quinn::SendStream> {
        self.connection
            .open_uni()
            .await
            .map_err(|e| MqttError::ConnectionError(format!("Failed to open QUIC stream: {e}")))
    }

    /// # Errors
    /// Returns an error if the stream cannot be opened or the flow header write fails.
    #[instrument(skip(self), fields(strategy = ?self.strategy), level = "debug")]
    pub async fn open_data_stream_with_flow(&self) -> Result<(quinn::SendStream, FlowId)> {
        let mut send = self.open_data_stream().await?;
        let flow_id = self.next_flow_id();
        if let Some(header) = self.flow_header(flow_id) {
            send.write_all(&header).await.map_err(|e| {
                MqttError::ConnectionError(format!("Failed to write flow header: {e}"))
            })?;
            debug!(flow_id = ?flow_id, "Wrote client data flow header on new stream");
        }
        Ok((send, flow_id))
    }

    /// # Errors
    /// Returns an error if the stream cannot be opened or the recovery header write fails.
    pub async fn open_recovery_stream(
        &self,
        flow_id: FlowId,
        recovery_flags: FlowFlags,
    ) -> Result<quinn::SendStream> {
        let mut send = self.open_data_stream().await?;

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

        Ok(send)
    }

    pub fn set_recovery_mode(&mut self, enable: bool) {
        self.flow_flags.clean = u8::from(!enable);
    }

    #[must_use]
    pub fn current_flow_flags(&self) -> FlowFlags {
        self.flow_flags
    }

    fn next_flow_id(&self) -> FlowId {
        if self.enable_flow_headers {
            self.flow_id_generator.lock().next_client()
        } else {
            FlowId::client(0)
        }
    }

    fn flow_header(&self, flow_id: FlowId) -> Option<BytesMut> {
        self.enable_flow_headers.then(|| {
            let mut buf = BytesMut::with_capacity(32);
            DataFlowHeader::client(flow_id, self.flow_expire_interval, self.flow_flags)
                .encode(&mut buf);
            buf
        })
    }

    fn unopened_stream(&self) -> (SharedWriter, FlowId) {
        let flow_id = self.next_flow_id();
        let writer = Arc::new(UnifiedWriter::quic_unopened(
            Arc::clone(&self.connection),
            Arc::clone(&self.drained),
            self.flow_header(flow_id),
        ));
        (writer, flow_id)
    }

    /// Sends a packet on a dedicated QUIC stream.
    ///
    /// # Errors
    /// Returns an error if the stream operation or packet encoding fails.
    #[instrument(skip(self, packet), level = "debug")]
    pub async fn send_packet_on_stream(&self, packet: Packet) -> Result<()> {
        self.prepare(&DataStream::Dedicated, packet_size(&packet))
            .await?
            .commit_then(&packet, || ())?
            .send()
            .await
    }

    #[must_use]
    pub fn strategy(&self) -> StreamStrategy {
        self.strategy
    }

    #[must_use]
    pub fn flow_headers_enabled(&self) -> bool {
        self.enable_flow_headers
    }

    pub(crate) async fn prepare(&self, target: &DataStream, size: usize) -> Result<PreparedStream> {
        match target {
            DataStream::Topic(topic) => {
                if self.closed.load(Ordering::Acquire) {
                    return Err(MqttError::NotConnected);
                }
                let info = match self.reserve_topic_stream(topic) {
                    Some(info) => info,
                    None => self.reserve_when_drained(topic).await?,
                };
                let reservation = Reservation {
                    info,
                    drained: Arc::clone(&self.drained),
                };
                let room = reservation.info.stream.reserve_room(size).await;
                Ok(PreparedStream::Topic(reservation, room))
            }
            DataStream::Dedicated => {
                let slot = Arc::clone(&self.dedicated_slots)
                    .acquire_owned()
                    .await
                    .map_err(|_| MqttError::NotConnected)?;
                let (writer, flow_id) = self.unopened_stream();
                writer.hold_slot(slot);
                let room = writer.reserve_room(size).await;
                Ok(PreparedStream::Dedicated(room, flow_id))
            }
        }
    }

    async fn reserve_when_drained(&self, topic: &str) -> Result<Arc<StreamInfo>> {
        loop {
            let drained = self.drained.notified();
            tokio::pin!(drained);
            drained.as_mut().enable();
            if self.closed.load(Ordering::Acquire) {
                return Err(MqttError::NotConnected);
            }
            if let Some(info) = self.reserve_topic_stream(topic) {
                return Ok(info);
            }
            trace!(topic = %topic, "Every cached topic stream is busy; waiting for one to drain");
            drained.await;
        }
    }

    fn reserve_topic_stream(&self, topic: &str) -> Option<Arc<StreamInfo>> {
        let mut streams = self.topic_streams.lock();
        let now = Instant::now();
        streams.retain(|idle_topic, info| {
            let idle = now.duration_since(*info.last_used.lock()) > self.stream_idle_timeout;
            let closed = idle && info.evictable();
            if closed {
                debug!(topic = %idle_topic, flow_id = ?info.flow_id, "Closed idle stream");
            }
            !closed
        });

        if let Some(info) = streams.get(topic) {
            info.reserved.fetch_add(1, Ordering::AcqRel);
            return Some(Arc::clone(info));
        }

        if streams.len() >= self.max_cached_streams {
            let mut by_age: Vec<(Instant, String)> = streams
                .iter()
                .map(|(cached, info)| (*info.last_used.lock(), cached.clone()))
                .collect();
            by_age.sort();
            let evicted = by_age.into_iter().find_map(|(_, cached)| {
                streams
                    .get(&cached)
                    .filter(|info| info.evictable())
                    .map(|_| cached)
            });
            let evicted = evicted?;
            streams.remove(&evicted);
            debug!(topic = %evicted, "Evicted oldest idle stream from cache (LRU)");
        }

        debug!(topic = %topic, "Opening new stream for topic");
        let (stream, flow_id) = self.unopened_stream();
        let info = Arc::new(StreamInfo {
            stream,
            flow_id,
            last_used: Mutex::new(Instant::now()),
            reserved: AtomicUsize::new(1),
        });
        streams.insert(topic.to_string(), Arc::clone(&info));
        Some(info)
    }

    /// Sends a packet on a topic-specific stream.
    ///
    /// # Errors
    /// Returns an error if the stream operation or packet encoding fails.
    pub async fn send_on_topic_stream(&self, topic: String, packet: Packet) -> Result<()> {
        self.prepare(&DataStream::Topic(topic), packet_size(&packet))
            .await?
            .commit_then(&packet, || ())?
            .send()
            .await
    }

    pub fn get_flow_id_for_topic(&self, topic: &str) -> Option<FlowId> {
        self.topic_streams
            .lock()
            .get(topic)
            .map(|info| info.flow_id)
    }

    #[instrument(skip(self, stream), level = "debug")]
    pub async fn register_flow_stream(&self, flow_id: FlowId, stream: SendStream) {
        self.flow_streams.lock().insert(
            flow_id,
            FlowStreamInfo {
                stream: Arc::new(UnifiedWriter::quic(stream)),
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
        let registered = self.flow_streams.lock().get_mut(&flow_id).map(|info| {
            info.last_used = Instant::now();
            Arc::clone(&info.stream)
        });
        let (stream, end) = if let Some(stream) = registered {
            let end = stream
                .reserve_room(packet_size(&packet))
                .await
                .commit_then(&packet, || ())?;
            (stream, end)
        } else {
            let (stream, new_flow_id) = self.unopened_stream();
            let end = stream.commit(&packet)?;
            self.flow_streams.lock().insert(
                new_flow_id,
                FlowStreamInfo {
                    stream: Arc::clone(&stream),
                    last_used: Instant::now(),
                },
            );
            debug!(
                requested_flow_id = ?flow_id,
                actual_flow_id = ?new_flow_id,
                "Flow not found, opened new stream"
            );
            (stream, end)
        };
        Flush::new(&stream, end).wait().await?;
        debug!(flow_id = ?flow_id, "Sent packet on flow stream");
        Ok(())
    }

    pub fn has_flow_stream(&self, flow_id: FlowId) -> bool {
        self.flow_streams.lock().contains_key(&flow_id)
    }

    pub async fn remove_flow_stream(&self, flow_id: FlowId) -> bool {
        let removed = self.flow_streams.lock().remove(&flow_id);
        if let Some(info) = removed {
            if let Err(e) = info.stream.finish_or_reset().await {
                debug!("QUIC data stream not finished: {e}");
            }
            debug!(flow_id = ?flow_id, "Removed flow stream");
            true
        } else {
            false
        }
    }

    /// # Errors
    /// Returns an error if flow headers are not enabled, the bi stream cannot be opened,
    /// or the peer does not respond within 2 seconds.
    #[instrument(skip(self), level = "debug")]
    pub async fn discard_flow(&self, flow_id: FlowId) -> Result<()> {
        if !self.enable_flow_headers {
            return Err(MqttError::ProtocolError(
                "flow headers not enabled, cannot discard flow".into(),
            ));
        }

        let (mut send, mut recv) = self.connection.open_bi().await.map_err(|e| {
            MqttError::ConnectionError(format!("failed to open bi stream for discard: {e}"))
        })?;

        let discard_flags = FlowFlags::discard();
        let header = DataFlowHeader::client(flow_id, 0, discard_flags);
        let mut buf = BytesMut::with_capacity(32);
        header.encode(&mut buf);

        send.write_all(&buf).await.map_err(|e| {
            MqttError::ConnectionError(format!("failed to write discard flow header: {e}"))
        })?;

        send.finish().map_err(|e| {
            MqttError::ConnectionError(format!("failed to finish discard send stream: {e}"))
        })?;

        let read_result = tokio::time::timeout(Duration::from_secs(2), recv.read_to_end(64)).await;

        match read_result {
            Ok(Ok(data)) => {
                if !data.is_empty() {
                    warn!(
                        flow_id = ?flow_id,
                        len = data.len(),
                        "peer returned non-empty data on discard response"
                    );
                }
            }
            Ok(Err(e)) => {
                return Err(MqttError::ConnectionError(format!(
                    "failed to read discard response: {e}"
                )));
            }
            Err(_) => {
                return Err(MqttError::Timeout);
            }
        }

        self.flow_streams.lock().remove(&flow_id);
        self.topic_streams
            .lock()
            .retain(|_, info| info.flow_id != flow_id);
        self.drained.notify_waiters();

        debug!(flow_id = ?flow_id, "discarded flow at peer and cleaned up local state");

        Ok(())
    }

    pub fn close_all_streams(&self) {
        self.closed.store(true, Ordering::Release);
        self.drained.notify_waiters();
        let topics: Vec<(String, Arc<StreamInfo>)> = self.topic_streams.lock().drain().collect();
        for (topic, info) in topics {
            info.stream.abandon();
            trace!(topic = %topic, flow_id = ?info.flow_id, "Closed topic stream");
        }

        let flows: Vec<(FlowId, FlowStreamInfo)> = self.flow_streams.lock().drain().collect();
        for (flow_id, info) in flows {
            info.stream.abandon();
            trace!(flow_id = ?flow_id, "Closed flow stream");
        }
    }
}

#[derive(Clone)]
pub(crate) enum DataStream {
    Topic(String),
    Dedicated,
}

pub(crate) struct Reservation {
    info: Arc<StreamInfo>,
    drained: Arc<Notify>,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.info.reserved.fetch_sub(1, Ordering::AcqRel);
        self.drained.notify_waiters();
    }
}

pub(crate) enum PreparedStream {
    Topic(Reservation, RoomReservation),
    Dedicated(RoomReservation, FlowId),
}

impl PreparedStream {
    pub(crate) fn commit_then(
        self,
        packet: &Packet,
        committed: impl FnOnce(),
    ) -> Result<CommittedPacket> {
        match self {
            Self::Topic(reservation, room) => {
                let end = room.commit_then(packet, committed)?;
                *reservation.info.last_used.lock() = Instant::now();
                trace!(flow_id = ?reservation.info.flow_id, "Committed packet to topic-specific stream");
                Ok(CommittedPacket {
                    flush: Flush::new(&reservation.info.stream, end),
                    finish: None,
                })
            }
            Self::Dedicated(room, flow_id) => {
                let writer = Arc::clone(room.writer());
                let end = room.commit_then(packet, committed)?;
                trace!(flow_id = ?flow_id, "Committed packet to dedicated QUIC stream");
                Ok(CommittedPacket {
                    flush: Flush::new(&writer, end),
                    finish: Some(writer),
                })
            }
        }
    }
}

pub(crate) struct CommittedPacket {
    flush: Flush,
    finish: Option<SharedWriter>,
}

impl CommittedPacket {
    pub(crate) async fn send(self) -> Result<()> {
        self.flush.wait().await?;
        if let Some(writer) = self.finish {
            writer.finish().await?;
            tokio::task::yield_now().await;
        }
        Ok(())
    }
}

pub(crate) fn packet_size(packet: &Packet) -> usize {
    match packet {
        Packet::Publish(publish) => publish.body_encoded_size() + 5,
        _ => 0,
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
    #[allow(deprecated)]
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
