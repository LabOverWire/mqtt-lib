use crate::broker::config::ServerDeliveryStrategy;
use crate::error::{MqttError, Result};
use crate::packet::Packet;
use crate::transport::flow::{DataFlowHeader, FlowFlags, FlowId, FlowIdGenerator};
use crate::QoS;
use bytes::BytesMut;
use quinn::{Connection, RecvStream, SendStream};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

/// Channel carrying packets read off a QUIC stream into the client handler,
/// tagged with the flow they arrived on.
pub(super) type PacketSender = mpsc::Sender<(Packet, Option<u64>)>;

struct ServerStreamInfo {
    stream: SendStream,
    flow_id: FlowId,
    last_used: Instant,
    /// Kept alive so the reader task keeps running for this flow's lifetime.
    ack_reader: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for ServerStreamInfo {
    fn drop(&mut self) {
        if let Some(handle) = self.ack_reader.take() {
            handle.abort();
        }
    }
}

/// Reads the client's acknowledgements off the recv half of a stream the broker opened.
///
/// `MQoQ` 9.1.2: PUBACK/PUBREL/PUBCOMP for a PUBLISH must be exchanged in the same data flow
/// as that PUBLISH. The broker therefore opens QoS>0 data flows as bidirectional streams and
/// must read the return direction; dropping the recv half instead makes `quinn` emit
/// `STOP_SENDING`, so the client's ack write fails and the handshake can never complete.
///
/// The client writes bare MQTT packets here (no flow header), so unlike a client-initiated
/// data stream there is no header to parse.
fn spawn_ack_reader(
    mut recv: RecvStream,
    flow_id: FlowId,
    packet_tx: PacketSender,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut buffer = BytesMut::new();
        loop {
            match crate::broker::quic_acceptor::read_packet_with_buffer(&mut recv, &mut buffer)
                .await
            {
                Ok(packet) => {
                    trace!(
                        flow_id = ?flow_id,
                        packet_type = %packet.packet_type_name(),
                        "Read ack from server data flow"
                    );
                    if packet_tx.send((packet, Some(flow_id.raw()))).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    trace!(flow_id = ?flow_id, "Server data flow ack reader ended: {e}");
                    break;
                }
            }
        }
    })
}

const MAX_CACHED_STREAMS: usize = 100;
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const FLOW_EXPIRE_INTERVAL: u64 = 300;

pub struct ServerStreamManager {
    connection: Arc<Connection>,
    strategy: ServerDeliveryStrategy,
    topic_streams: HashMap<String, ServerStreamInfo>,
    flow_streams: HashMap<u64, ServerStreamInfo>,
    flow_id_generator: FlowIdGenerator,
    header_buffer: BytesMut,
    /// Feeds acks read off server data flows back into the client handler.
    packet_tx: Option<PacketSender>,
}

impl ServerStreamManager {
    pub fn new(connection: Arc<Connection>) -> Self {
        Self {
            connection,
            strategy: ServerDeliveryStrategy::default(),
            topic_streams: HashMap::new(),
            flow_streams: HashMap::new(),
            flow_id_generator: FlowIdGenerator::new(),
            header_buffer: BytesMut::with_capacity(32),
            packet_tx: None,
        }
    }

    /// Supplies the channel used to deliver acks read off server data flows.
    ///
    /// Without it, QoS>0 delivery on a data flow cannot complete its handshake.
    #[must_use]
    pub(super) fn with_packet_tx(mut self, packet_tx: PacketSender) -> Self {
        self.packet_tx = Some(packet_tx);
        self
    }

    /// Starts reading acks off a stream the broker opened, per `MQoQ` 9.1.2.
    fn start_ack_reader(
        &self,
        recv: RecvStream,
        flow_id: FlowId,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let Some(tx) = self.packet_tx.clone() else {
            warn!(
                flow_id = ?flow_id,
                "No packet channel for server data flow; QoS>0 acks on this flow cannot be read"
            );
            return None;
        };
        Some(spawn_ack_reader(recv, flow_id, tx))
    }

    #[must_use]
    pub fn with_strategy(mut self, strategy: ServerDeliveryStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    pub async fn write_publish(
        &mut self,
        topic: &str,
        encoded_packet: &[u8],
        qos: QoS,
    ) -> Result<()> {
        match self.strategy {
            ServerDeliveryStrategy::ControlOnly => Err(MqttError::ConnectionError(
                "control-only delivery: caller should write to control stream directly".to_string(),
            )),
            ServerDeliveryStrategy::PerTopic => {
                self.write_on_topic_stream(topic, encoded_packet).await
            }
            ServerDeliveryStrategy::PerPublish => {
                self.write_on_ephemeral_stream(topic, encoded_packet, qos)
                    .await
            }
        }
    }

    pub async fn write_publish_to_flow(
        &mut self,
        flow_id: u64,
        encoded_packet: &[u8],
    ) -> Result<()> {
        if let Some(info) = self.flow_streams.get_mut(&flow_id) {
            info.last_used = Instant::now();
            trace!(flow_id = flow_id, "Reusing server stream for flow");
            return write_to_stream(&mut info.stream, encoded_packet).await;
        }

        let (mut send, recv) = self.connection.open_bi().await.map_err(|e| {
            MqttError::ConnectionError(format!("failed to open server QUIC stream for flow: {e}"))
        })?;

        let fid = FlowId::from(flow_id);
        let ack_reader = self.start_ack_reader(recv, fid);

        self.header_buffer.clear();
        let header = DataFlowHeader::server(fid, FLOW_EXPIRE_INTERVAL, FlowFlags::default());
        header.encode(&mut self.header_buffer);

        send.write_all(&self.header_buffer).await.map_err(|e| {
            MqttError::ConnectionError(format!("failed to write server flow header: {e}"))
        })?;

        debug!(
            flow_id = flow_id,
            "Opened new server stream for flow-bound subscription"
        );

        write_to_stream(&mut send, encoded_packet).await?;

        self.flow_streams.insert(
            flow_id,
            ServerStreamInfo {
                stream: send,
                flow_id: fid,
                last_used: Instant::now(),
                ack_reader,
            },
        );

        Ok(())
    }

    pub fn remove_flow_stream(&mut self, flow_id: u64) {
        if let Some(mut info) = self.flow_streams.remove(&flow_id) {
            let _ = info.stream.finish();
            debug!(flow_id = flow_id, "Closed server stream for flow");
        }
    }

    async fn write_on_topic_stream(&mut self, topic: &str, encoded_packet: &[u8]) -> Result<()> {
        self.evict_idle_streams();

        if let Some(info) = self.topic_streams.get_mut(topic) {
            info.last_used = Instant::now();
            trace!(topic = %topic, flow_id = ?info.flow_id, "Reusing server stream for topic");
            return write_to_stream(&mut info.stream, encoded_packet).await;
        }

        if self.topic_streams.len() >= MAX_CACHED_STREAMS {
            self.evict_lru_stream();
        }

        let (mut send, recv) = self.connection.open_bi().await.map_err(|e| {
            MqttError::ConnectionError(format!("failed to open server QUIC stream: {e}"))
        })?;

        let flow_id = self.flow_id_generator.next_server();
        let ack_reader = self.start_ack_reader(recv, flow_id);

        self.header_buffer.clear();
        let header = DataFlowHeader::server(flow_id, FLOW_EXPIRE_INTERVAL, FlowFlags::default());
        header.encode(&mut self.header_buffer);

        send.write_all(&self.header_buffer).await.map_err(|e| {
            MqttError::ConnectionError(format!("failed to write server flow header: {e}"))
        })?;

        debug!(topic = %topic, flow_id = ?flow_id, "Opened new server stream for topic");

        write_to_stream(&mut send, encoded_packet).await?;

        self.topic_streams.insert(
            topic.to_string(),
            ServerStreamInfo {
                stream: send,
                flow_id,
                last_used: Instant::now(),
                ack_reader,
            },
        );

        Ok(())
    }

    async fn write_on_ephemeral_stream(
        &mut self,
        topic: &str,
        encoded_packet: &[u8],
        qos: QoS,
    ) -> Result<()> {
        let flow_id = self.flow_id_generator.next_server();

        // QoS0 needs no ack, so a one-way flow suffices. QoS>0 must be able to receive
        // PUBACK/PUBREC on the same flow (MQoQ 9.1.2), hence a bidirectional stream whose
        // recv half is read for the life of the flow. The reader is detached because an
        // ephemeral stream is not cached; it ends when the peer closes or errors.
        let mut send = if qos == QoS::AtMostOnce {
            self.connection.open_uni().await.map_err(|e| {
                MqttError::ConnectionError(format!("failed to open server QUIC stream: {e}"))
            })?
        } else {
            let (send, recv) = self.connection.open_bi().await.map_err(|e| {
                MqttError::ConnectionError(format!("failed to open server QUIC stream: {e}"))
            })?;
            drop(self.start_ack_reader(recv, flow_id));
            send
        };

        self.header_buffer.clear();
        let header = DataFlowHeader::server(flow_id, FLOW_EXPIRE_INTERVAL, FlowFlags::default());
        header.encode(&mut self.header_buffer);

        send.write_all(&self.header_buffer).await.map_err(|e| {
            MqttError::ConnectionError(format!("failed to write server flow header: {e}"))
        })?;

        write_to_stream(&mut send, encoded_packet).await?;

        let _ = send.finish();

        tokio::task::yield_now().await;

        debug!(topic = %topic, flow_id = ?flow_id, "Sent publish on ephemeral server stream");

        Ok(())
    }

    fn evict_idle_streams(&mut self) {
        let now = Instant::now();
        self.topic_streams.retain(|topic, info| {
            if now.duration_since(info.last_used) > STREAM_IDLE_TIMEOUT {
                let _ = info.stream.finish();
                debug!(topic = %topic, flow_id = ?info.flow_id, "Closed idle server stream");
                false
            } else {
                true
            }
        });
    }

    fn evict_lru_stream(&mut self) {
        let oldest = self
            .topic_streams
            .iter()
            .min_by_key(|(_, info)| info.last_used)
            .map(|(k, _)| k.clone());

        if let Some(oldest_topic) = oldest {
            if let Some(mut info) = self.topic_streams.remove(&oldest_topic) {
                let _ = info.stream.finish();
                debug!(
                    topic = %oldest_topic,
                    flow_id = ?info.flow_id,
                    "Evicted LRU server stream"
                );
            }
        }
    }

    pub fn close_all_streams(&mut self) {
        for (topic, mut info) in self.topic_streams.drain() {
            let _ = info.stream.finish();
            trace!(topic = %topic, flow_id = ?info.flow_id, "Closed server stream");
        }
        for (raw_id, mut info) in self.flow_streams.drain() {
            let _ = info.stream.finish();
            trace!(flow_id = raw_id, "Closed flow-bound server stream");
        }
    }
}

impl Drop for ServerStreamManager {
    fn drop(&mut self) {
        self.close_all_streams();
    }
}

async fn write_to_stream(stream: &mut SendStream, data: &[u8]) -> Result<()> {
    stream
        .write_all(data)
        .await
        .map_err(|e| MqttError::ConnectionError(format!("QUIC server stream write error: {e}")))
}
