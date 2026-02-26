use crate::error::{MqttError, Result};
use crate::transport::flow::{DataFlowHeader, FlowFlags, FlowId, FlowIdGenerator};
use bytes::BytesMut;
use quinn::{Connection, SendStream};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, trace};

struct ServerStreamInfo {
    stream: SendStream,
    flow_id: FlowId,
    last_used: Instant,
}

const MAX_CACHED_STREAMS: usize = 100;
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const FLOW_EXPIRE_INTERVAL: u64 = 300;

pub struct ServerStreamManager {
    connection: Arc<Connection>,
    topic_streams: HashMap<String, ServerStreamInfo>,
    flow_id_generator: FlowIdGenerator,
    header_buffer: BytesMut,
}

impl ServerStreamManager {
    pub fn new(connection: Arc<Connection>) -> Self {
        Self {
            connection,
            topic_streams: HashMap::new(),
            flow_id_generator: FlowIdGenerator::new(),
            header_buffer: BytesMut::with_capacity(32),
        }
    }

    pub async fn write_publish(&mut self, topic: &str, encoded_packet: &[u8]) -> Result<()> {
        self.evict_idle_streams();

        if let Some(info) = self.topic_streams.get_mut(topic) {
            info.last_used = Instant::now();
            trace!(topic = %topic, flow_id = ?info.flow_id, "Reusing server stream for topic");
            return write_to_stream(&mut info.stream, encoded_packet).await;
        }

        if self.topic_streams.len() >= MAX_CACHED_STREAMS {
            self.evict_lru_stream();
        }

        let (mut send, _recv) = self.connection.open_bi().await.map_err(|e| {
            MqttError::ConnectionError(format!("failed to open server QUIC stream: {e}"))
        })?;

        let flow_id = self.flow_id_generator.next_server();

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
            },
        );

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
