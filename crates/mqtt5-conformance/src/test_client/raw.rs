//! Raw-TCP backend for [`crate::test_client::TestClient`].
//!
//! Speaks MQTT v5 directly over a [`crate::transport::TcpTransport`] with
//! its own packet reader task and `QoS` state machines. This is the path the
//! standalone conformance runner takes when validating third-party brokers
//! described by a [`crate::sut::SutDescriptor`].
//!
//! Both the in-process backend and the raw backend converge on the same
//! [`crate::test_client::ReceivedMessage`] / [`crate::test_client::Subscription`]
//! types, so test bodies remain agnostic of which one they execute against.

use super::{MessageQueue, ReceivedMessage, Subscription, TestClientError};
use crate::transport::TcpTransport;
use bytes::BytesMut;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::pubrel::PubRelPacket;
use mqtt5_protocol::packet::subscribe::{SubscribePacket, TopicFilter};
use mqtt5_protocol::packet::subscribe_options::{
    RetainHandling as PacketRetainHandling, SubscriptionOptions,
};
use mqtt5_protocol::packet::unsubscribe::UnsubscribePacket;
use mqtt5_protocol::packet::{
    puback::PubAckPacket, pubcomp::PubCompPacket, pubrec::PubRecPacket, FixedHeader, MqttPacket,
    Packet,
};
use mqtt5_protocol::types::{
    ConnectOptions, PublishOptions, QoS, RetainHandling, SubscribeOptions,
};
use mqtt5_protocol::validation::{strip_shared_subscription_prefix, topic_matches_filter};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio::task::JoinHandle;

const READ_BUF_INITIAL: usize = 8 * 1024;
const ACK_TIMEOUT: Duration = Duration::from_secs(10);

type AckMap = Arc<StdMutex<HashMap<u16, oneshot::Sender<AckOutcome>>>>;
type SubscriptionList = Arc<StdMutex<Vec<SubscriptionEntry>>>;
type ConnAckSlot = Arc<StdMutex<Option<ConnAckInfo>>>;

#[derive(Debug, Clone, Copy)]
struct ConnAckInfo {
    session_present: bool,
}

#[derive(Debug)]
enum AckOutcome {
    SubAck(Vec<mqtt5_protocol::packet::suback::SubAckReasonCode>),
    UnsubAck(Vec<mqtt5_protocol::packet::unsuback::UnsubAckReasonCode>),
    PubAck(mqtt5_protocol::types::ReasonCode),
    PubRec(mqtt5_protocol::types::ReasonCode),
    PubComp(mqtt5_protocol::types::ReasonCode),
}

struct SubscriptionEntry {
    filter: String,
    queue: MessageQueue,
}

struct SharedState {
    writer: AsyncMutex<Option<OwnedWriteHalf>>,
    pending_acks: AckMap,
    subscriptions: SubscriptionList,
    connack: ConnAckSlot,
}

/// Raw-TCP backing for [`crate::test_client::TestClient`].
pub struct RawTestClient {
    client_id: String,
    next_packet_id: AtomicU16,
    shared: Arc<SharedState>,
    reader_task: StdMutex<Option<JoinHandle<()>>>,
    session_present: bool,
}

impl RawTestClient {
    /// Connects a raw client to `addr` using the given protocol-level
    /// connect options.
    ///
    /// # Errors
    /// Returns an error if the TCP connection fails, the CONNECT handshake
    /// is rejected, or the broker returns a malformed CONNACK.
    ///
    /// # Panics
    /// Panics if the internal reader-handle mutex is poisoned, which can
    /// only happen if a prior holder panicked while holding the lock.
    pub async fn connect(
        addr: SocketAddr,
        options: ConnectOptions,
    ) -> Result<Self, TestClientError> {
        let client_id = options.client_id.clone();
        let stream = TcpTransport::connect(addr).await?.into_inner();
        let (read_half, mut write_half) = stream.into_split();

        let connect = ConnectPacket::new(options);
        let mut buf = BytesMut::with_capacity(64);
        connect.encode(&mut buf)?;
        write_half.write_all(&buf).await?;
        write_half.flush().await?;

        let pending_acks: AckMap = Arc::new(StdMutex::new(HashMap::new()));
        let subscriptions: SubscriptionList = Arc::new(StdMutex::new(Vec::new()));
        let connack: ConnAckSlot = Arc::new(StdMutex::new(None));
        let shared = Arc::new(SharedState {
            writer: AsyncMutex::new(Some(write_half)),
            pending_acks: Arc::clone(&pending_acks),
            subscriptions: Arc::clone(&subscriptions),
            connack: Arc::clone(&connack),
        });

        let reader_shared = Arc::clone(&shared);
        let reader_task = tokio::spawn(async move {
            let _ = reader_loop(read_half, reader_shared).await;
        });

        let reader_handle_for_wait = StdMutex::new(Some(reader_task));
        let session_present = Self::await_connack_static(&shared, &reader_handle_for_wait).await?;
        let reader_task = reader_handle_for_wait.lock().unwrap().take();

        Ok(Self {
            client_id,
            next_packet_id: AtomicU16::new(1),
            shared,
            reader_task: StdMutex::new(reader_task),
            session_present,
        })
    }

    async fn await_connack_static(
        shared: &Arc<SharedState>,
        reader_task: &StdMutex<Option<JoinHandle<()>>>,
    ) -> Result<bool, TestClientError> {
        let deadline = tokio::time::Instant::now() + ACK_TIMEOUT;
        loop {
            {
                let guard = shared.connack.lock().unwrap();
                if let Some(info) = *guard {
                    return Ok(info.session_present);
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(TestClientError::Timeout("connack"));
            }
            if reader_task
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(JoinHandle::is_finished)
            {
                return Err(TestClientError::Disconnected);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Returns the client id used to connect.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Returns the `SessionPresent` flag from the server's CONNACK.
    #[must_use]
    pub fn session_present(&self) -> bool {
        self.session_present
    }

    fn allocate_packet_id(&self) -> u16 {
        loop {
            let id = self.next_packet_id.fetch_add(1, Ordering::Relaxed);
            if id != 0 {
                return id;
            }
        }
    }

    async fn write_packet<P: MqttPacket>(&self, packet: &P) -> Result<(), TestClientError> {
        let mut buf = BytesMut::with_capacity(64);
        packet.encode(&mut buf)?;
        let mut writer = self.shared.writer.lock().await;
        let stream = writer.as_mut().ok_or(TestClientError::Disconnected)?;
        stream.write_all(&buf).await?;
        stream.flush().await?;
        Ok(())
    }

    async fn await_ack(
        &self,
        packet_id: u16,
        op: &'static str,
    ) -> Result<AckOutcome, TestClientError> {
        let (tx, rx) = oneshot::channel();
        {
            let mut acks = self.shared.pending_acks.lock().unwrap();
            acks.insert(packet_id, tx);
        }
        match tokio::time::timeout(ACK_TIMEOUT, rx).await {
            Ok(Ok(outcome)) => Ok(outcome),
            Ok(Err(_)) => Err(TestClientError::Disconnected),
            Err(_) => {
                let mut acks = self.shared.pending_acks.lock().unwrap();
                acks.remove(&packet_id);
                Err(TestClientError::Timeout(op))
            }
        }
    }

    /// Publishes `payload` to `topic` with default options (`QoS` 0, no retain).
    ///
    /// # Errors
    /// Returns an error if the broker rejects the publish or the connection
    /// is closed.
    pub async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), TestClientError> {
        self.publish_with_options(topic, payload, PublishOptions::default())
            .await
    }

    /// Publishes with the given [`PublishOptions`].
    ///
    /// # Errors
    /// Returns an error if the broker rejects the publish, the connection
    /// is closed, or the `QoS` handshake times out.
    pub async fn publish_with_options(
        &self,
        topic: &str,
        payload: &[u8],
        options: PublishOptions,
    ) -> Result<(), TestClientError> {
        let qos = options.qos;
        let packet_id = match qos {
            QoS::AtMostOnce => 0,
            QoS::AtLeastOnce | QoS::ExactlyOnce => self.allocate_packet_id(),
        };

        let mut publish = PublishPacket::new(topic, payload.to_vec(), qos);
        if qos != QoS::AtMostOnce {
            publish = publish.with_packet_id(packet_id);
        }
        if options.retain {
            publish = publish.with_retain(true);
        }
        if let Some(expiry) = options.properties.message_expiry_interval {
            publish.properties.set_message_expiry_interval(expiry);
        }
        if let Some(format) = options.properties.payload_format_indicator {
            publish.properties.set_payload_format_indicator(format);
        }
        if let Some(content_type) = options.properties.content_type {
            publish.properties.set_content_type(content_type);
        }
        if let Some(response_topic) = options.properties.response_topic {
            publish.properties.set_response_topic(response_topic);
        }
        if let Some(correlation) = options.properties.correlation_data {
            publish.properties.set_correlation_data(correlation.into());
        }
        for (key, value) in options.properties.user_properties {
            publish.properties.add_user_property(key, value);
        }
        if let Some(topic_alias) = options.properties.topic_alias {
            publish.properties.set_topic_alias(topic_alias);
        }
        for sub_id in options.properties.subscription_identifiers {
            publish.properties.set_subscription_identifier(sub_id);
        }

        self.write_packet(&publish).await?;

        match qos {
            QoS::AtMostOnce => Ok(()),
            QoS::AtLeastOnce => match self.await_ack(packet_id, "puback").await? {
                AckOutcome::PubAck(rc) => {
                    if rc.is_success() {
                        Ok(())
                    } else {
                        Err(TestClientError::Unexpected(format!(
                            "PUBACK reason code {rc:?}"
                        )))
                    }
                }
                other => Err(TestClientError::Unexpected(format!(
                    "expected PUBACK, got {other:?}"
                ))),
            },
            QoS::ExactlyOnce => {
                match self.await_ack(packet_id, "pubrec").await? {
                    AckOutcome::PubRec(rc) if rc.is_success() => {}
                    AckOutcome::PubRec(rc) => {
                        return Err(TestClientError::Unexpected(format!(
                            "PUBREC reason code {rc:?}"
                        )));
                    }
                    other => {
                        return Err(TestClientError::Unexpected(format!(
                            "expected PUBREC, got {other:?}"
                        )));
                    }
                }
                let pubrel = PubRelPacket::new(packet_id);
                self.write_packet(&pubrel).await?;
                match self.await_ack(packet_id, "pubcomp").await? {
                    AckOutcome::PubComp(rc) if rc.is_success() => Ok(()),
                    AckOutcome::PubComp(rc) => Err(TestClientError::Unexpected(format!(
                        "PUBCOMP reason code {rc:?}"
                    ))),
                    other => Err(TestClientError::Unexpected(format!(
                        "expected PUBCOMP, got {other:?}"
                    ))),
                }
            }
        }
    }

    /// Subscribes to `filter` and returns a [`Subscription`] handle.
    ///
    /// # Errors
    /// Returns an error if the broker rejects the subscription or returns
    /// a non-success SUBACK reason code.
    ///
    /// # Panics
    /// Panics if the internal subscription mutex has been poisoned.
    pub async fn subscribe(
        &self,
        filter: &str,
        options: SubscribeOptions,
    ) -> Result<Subscription, TestClientError> {
        let messages: MessageQueue = Arc::new(StdMutex::new(Vec::new()));
        let matching_filter = strip_shared_subscription_prefix(filter);
        {
            let mut subs = self.shared.subscriptions.lock().unwrap();
            subs.push(SubscriptionEntry {
                filter: matching_filter.to_string(),
                queue: Arc::clone(&messages),
            });
        }

        let packet_id = self.allocate_packet_id();
        let sub_options = SubscriptionOptions {
            qos: options.qos,
            no_local: options.no_local,
            retain_as_published: options.retain_as_published,
            retain_handling: convert_retain_handling(options.retain_handling),
        };
        let topic_filter = TopicFilter::with_options(filter.to_string(), sub_options);
        let mut subscribe = SubscribePacket::new(packet_id).add_filter_with_options(topic_filter);
        if let Some(sub_id) = options.subscription_identifier {
            subscribe = subscribe.with_subscription_identifier(sub_id);
        }

        self.write_packet(&subscribe).await?;

        let outcome = self.await_ack(packet_id, "suback").await?;
        let granted_qos = match outcome {
            AckOutcome::SubAck(codes) => {
                let first = codes.first().copied().ok_or_else(|| {
                    TestClientError::Unexpected("SUBACK with empty reason codes".into())
                })?;
                first.granted_qos().ok_or_else(|| {
                    TestClientError::Unexpected(format!("SUBACK reason code {first:?}"))
                })?
            }
            other => {
                return Err(TestClientError::Unexpected(format!(
                    "expected SUBACK, got {other:?}"
                )));
            }
        };

        Ok(Subscription::new(messages, packet_id, granted_qos))
    }

    /// Unsubscribes from `filter`.
    ///
    /// # Errors
    /// Returns an error if the broker rejects the unsubscribe or returns
    /// a non-success UNSUBACK reason code.
    ///
    /// # Panics
    /// Panics if the internal subscription mutex has been poisoned.
    pub async fn unsubscribe(&self, filter: &str) -> Result<(), TestClientError> {
        {
            let mut subs = self.shared.subscriptions.lock().unwrap();
            subs.retain(|entry| entry.filter != filter);
        }

        let packet_id = self.allocate_packet_id();
        let unsubscribe = UnsubscribePacket::new(packet_id).add_filter(filter.to_string());
        self.write_packet(&unsubscribe).await?;

        match self.await_ack(packet_id, "unsuback").await? {
            AckOutcome::UnsubAck(codes) => {
                if codes
                    .iter()
                    .all(mqtt5_protocol::packet::unsuback::UnsubAckReasonCode::is_success)
                {
                    Ok(())
                } else {
                    Err(TestClientError::Unexpected(format!(
                        "UNSUBACK reason codes {codes:?}"
                    )))
                }
            }
            other => Err(TestClientError::Unexpected(format!(
                "expected UNSUBACK, got {other:?}"
            ))),
        }
    }

    /// Performs a normal DISCONNECT handshake.
    ///
    /// # Errors
    /// Returns an error if writing the DISCONNECT packet fails.
    pub async fn disconnect(&self) -> Result<(), TestClientError> {
        let disconnect = DisconnectPacket::normal();
        let mut buf = BytesMut::with_capacity(8);
        disconnect.encode(&mut buf)?;
        let mut writer = self.shared.writer.lock().await;
        if let Some(stream) = writer.as_mut() {
            let _ = stream.write_all(&buf).await;
            let _ = stream.flush().await;
            let _ = stream.shutdown().await;
        }
        *writer = None;
        drop(writer);
        self.stop_reader();
        Ok(())
    }

    /// Drops the TCP connection without sending a DISCONNECT packet.
    ///
    /// # Errors
    /// Never returns an error in the current implementation.
    pub async fn disconnect_abnormally(&self) -> Result<(), TestClientError> {
        let mut writer = self.shared.writer.lock().await;
        if let Some(mut stream) = writer.take() {
            let _ = stream.shutdown().await;
        }
        drop(writer);
        self.stop_reader();
        Ok(())
    }

    fn stop_reader(&self) {
        if let Some(handle) = self.reader_task.lock().unwrap().take() {
            handle.abort();
        }
    }
}

impl Drop for RawTestClient {
    fn drop(&mut self) {
        if let Ok(mut writer) = self.shared.writer.try_lock() {
            drop(writer.take());
        }
        self.stop_reader();
    }
}

fn convert_retain_handling(handling: RetainHandling) -> PacketRetainHandling {
    match handling {
        RetainHandling::SendAtSubscribe => PacketRetainHandling::SendAtSubscribe,
        RetainHandling::SendIfNew => PacketRetainHandling::SendAtSubscribeIfNew,
        RetainHandling::DontSend => PacketRetainHandling::DoNotSend,
    }
}

async fn reader_loop(
    mut reader: OwnedReadHalf,
    shared: Arc<SharedState>,
) -> Result<(), TestClientError> {
    let mut buf = BytesMut::with_capacity(READ_BUF_INITIAL);
    loop {
        let Ok(packet) = read_packet(&mut reader, &mut buf).await else {
            drop_pending_acks(&shared.pending_acks);
            return Ok(());
        };
        if let Err(err) = dispatch_packet(packet, &shared).await {
            drop_pending_acks(&shared.pending_acks);
            return Err(err);
        }
    }
}

async fn read_packet(
    reader: &mut OwnedReadHalf,
    buf: &mut BytesMut,
) -> Result<Packet, TestClientError> {
    loop {
        if let Some(packet) = try_parse_packet(buf)? {
            return Ok(packet);
        }
        let mut chunk = [0u8; 4096];
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            return Err(TestClientError::Disconnected);
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn try_parse_packet(buf: &mut BytesMut) -> Result<Option<Packet>, TestClientError> {
    if buf.is_empty() {
        return Ok(None);
    }
    let mut peek = &buf[..];
    let fixed_header = match FixedHeader::decode(&mut peek) {
        Ok(fh) => fh,
        Err(mqtt5_protocol::error::MqttError::MalformedPacket(msg))
            if msg.contains("fixed header") || msg.contains("Variable") =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err.into()),
    };
    let header_len = buf.len() - peek.len();
    let total = header_len + fixed_header.remaining_length as usize;
    if buf.len() < total {
        return Ok(None);
    }
    let _ = buf.split_to(header_len);
    let mut body = buf.split_to(fixed_header.remaining_length as usize);
    let packet = Packet::decode_from_body(fixed_header.packet_type, &fixed_header, &mut body)?;
    Ok(Some(packet))
}

async fn dispatch_packet(packet: Packet, shared: &SharedState) -> Result<(), TestClientError> {
    match packet {
        Packet::ConnAck(connack) => {
            {
                let mut slot = shared.connack.lock().unwrap();
                *slot = Some(ConnAckInfo {
                    session_present: connack.session_present,
                });
            }
            if !connack.reason_code.is_success() {
                return Err(TestClientError::Unexpected(format!(
                    "CONNACK reason code {:?}",
                    connack.reason_code
                )));
            }
        }
        Packet::Publish(publish) => {
            handle_publish(publish, shared).await?;
        }
        Packet::PubAck(puback) => {
            complete_ack(
                &shared.pending_acks,
                puback.packet_id,
                AckOutcome::PubAck(puback.reason_code),
            );
        }
        Packet::PubRec(pubrec) => {
            complete_ack(
                &shared.pending_acks,
                pubrec.packet_id,
                AckOutcome::PubRec(pubrec.reason_code),
            );
        }
        Packet::PubRel(pubrel) => {
            let pubcomp = PubCompPacket::new(pubrel.packet_id);
            send_raw(shared, &pubcomp).await?;
        }
        Packet::PubComp(pubcomp) => {
            complete_ack(
                &shared.pending_acks,
                pubcomp.packet_id,
                AckOutcome::PubComp(pubcomp.reason_code),
            );
        }
        Packet::SubAck(suback) => {
            complete_ack(
                &shared.pending_acks,
                suback.packet_id,
                AckOutcome::SubAck(suback.reason_codes),
            );
        }
        Packet::UnsubAck(unsuback) => {
            complete_ack(
                &shared.pending_acks,
                unsuback.packet_id,
                AckOutcome::UnsubAck(unsuback.reason_codes),
            );
        }
        Packet::Disconnect(_) => return Err(TestClientError::Disconnected),
        _ => {}
    }
    Ok(())
}

async fn handle_publish(
    publish: PublishPacket,
    shared: &SharedState,
) -> Result<(), TestClientError> {
    let received = ReceivedMessage::from_publish(&publish);
    let topic = publish.topic_name.clone();
    let qos = publish.qos;
    let packet_id = publish.packet_id;

    let entries: Vec<MessageQueue> = {
        let subs = shared.subscriptions.lock().unwrap();
        subs.iter()
            .filter(|entry| topic_matches_filter(&topic, &entry.filter))
            .map(|entry| Arc::clone(&entry.queue))
            .collect()
    };

    for queue in entries {
        queue.lock().unwrap().push(received.clone());
    }

    match qos {
        QoS::AtMostOnce => {}
        QoS::AtLeastOnce => {
            if let Some(id) = packet_id {
                let puback = PubAckPacket::new(id);
                send_raw(shared, &puback).await?;
            }
        }
        QoS::ExactlyOnce => {
            if let Some(id) = packet_id {
                let pubrec = PubRecPacket::new(id);
                send_raw(shared, &pubrec).await?;
            }
        }
    }

    Ok(())
}

async fn send_raw<P: MqttPacket>(shared: &SharedState, packet: &P) -> Result<(), TestClientError> {
    let mut buf = BytesMut::with_capacity(32);
    packet.encode(&mut buf)?;
    let mut writer = shared.writer.lock().await;
    let stream = writer.as_mut().ok_or(TestClientError::Disconnected)?;
    stream.write_all(&buf).await?;
    stream.flush().await?;
    Ok(())
}

fn complete_ack(acks: &AckMap, packet_id: u16, outcome: AckOutcome) {
    if let Some(tx) = acks.lock().unwrap().remove(&packet_id) {
        let _ = tx.send(outcome);
    }
}

fn drop_pending_acks(acks: &AckMap) {
    let mut guard = acks.lock().unwrap();
    guard.clear();
}

#[cfg(all(test, feature = "inprocess-fixture"))]
mod tests {
    use super::*;
    use crate::sut::InProcessFixture;
    use std::time::Duration;
    use ulid::Ulid;

    #[tokio::test]
    async fn raw_qos0_roundtrip() {
        let fixture = InProcessFixture::start().await;
        let addr = fixture.tcp_socket_addr();

        let sub_opts = ConnectOptions::new(format!("conf-raw-test-sub-{}", Ulid::new()));
        let subscriber = RawTestClient::connect(addr, sub_opts).await.unwrap();
        let subscription = subscriber
            .subscribe("raw/test/qos0", SubscribeOptions::default())
            .await
            .unwrap();

        let pub_opts = ConnectOptions::new(format!("conf-raw-test-pub-{}", Ulid::new()));
        let publisher = RawTestClient::connect(addr, pub_opts).await.unwrap();
        publisher
            .publish_with_options("raw/test/qos0", b"hello", PublishOptions::default())
            .await
            .unwrap();

        let msg = subscription
            .expect_publish(Duration::from_secs(3))
            .await
            .expect("message should arrive");
        assert_eq!(msg.topic, "raw/test/qos0");
        assert_eq!(msg.payload, b"hello");

        subscriber.disconnect().await.unwrap();
        publisher.disconnect().await.unwrap();
    }
}
