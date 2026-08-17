//! Packet type handlers for incoming packets

use crate::callback::CallbackManager;
use crate::codec::CodecRegistry;
use crate::error::{MqttError, Result};
use crate::packet::publish::PublishPacket;
use crate::packet::Packet;
use crate::protocol::v5::properties::Properties;
use crate::session::state::AckResolution;
use crate::session::SessionState;
use crate::transport::PacketWriter;
use crate::QoS;
use parking_lot::Mutex;
use std::sync::Arc;

use super::ack::{AckCallbackManager, AckDispatcher, AckKind, AckPublishCallback};
use super::keepalive::KeepaliveState;
use super::unified::UnifiedWriter;
#[cfg(feature = "transport-quic")]
use crate::transport::flow::FlowId;

/// Borrowed handles for deferred-acknowledgement delivery, threaded from the reader
/// context. Present only when `deferred_ack` is enabled connection-wide.
pub(super) struct AckDelivery<'a> {
    pub(super) callbacks: &'a Arc<AckCallbackManager>,
    pub(super) dispatcher: &'a Arc<AckDispatcher>,
}

/// Connection-scoped handles the incoming-packet handler needs, bundled from the
/// reader context so the entry point stays a small number of arguments.
pub(super) struct IncomingHandlers<'a> {
    pub(super) session: &'a Arc<tokio::sync::RwLock<SessionState>>,
    pub(super) callback_manager: &'a Arc<CallbackManager>,
    pub(super) keepalive_state: &'a Arc<Mutex<KeepaliveState>>,
    pub(super) codec_registry: Option<&'a Arc<CodecRegistry>>,
    pub(super) ack_delivery: Option<&'a AckDelivery<'a>>,
}

pub(super) async fn handle_incoming_packet_with_writer(
    packet: Packet,
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    flow_id: Option<crate::transport::flow::FlowId>,
    handlers: &IncomingHandlers<'_>,
) -> Result<()> {
    let session = handlers.session;
    match packet {
        Packet::Publish(publish) => {
            handle_publish_with_ack(
                publish,
                writer,
                session,
                handlers.callback_manager,
                flow_id,
                handlers.codec_registry,
                handlers.ack_delivery,
            )
            .await
        }
        Packet::PingResp => {
            handlers.keepalive_state.lock().record_pong_received();
            Ok(())
        }
        Packet::PubRec(pubrec) => handle_pubrec_outgoing(pubrec, writer, session).await,
        Packet::PubRel(pubrel) => handle_pubrel(pubrel, writer, session).await,
        Packet::PubComp(pubcomp) => handle_pubcomp_outgoing(pubcomp, session).await,
        Packet::Disconnect(disconnect) => {
            tracing::info!("Server sent DISCONNECT: {:?}", disconnect.reason_code);
            Err(MqttError::ConnectionError(
                "Server disconnected".to_string(),
            ))
        }
        _ => Ok(()),
    }
}

pub(super) async fn handle_publish_with_ack(
    mut publish: crate::packet::publish::PublishPacket,
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
    callback_manager: &Arc<CallbackManager>,
    flow_id: Option<crate::transport::flow::FlowId>,
    codec_registry: Option<&Arc<CodecRegistry>>,
    ack_delivery: Option<&AckDelivery<'_>>,
) -> Result<()> {
    if let Some(registry) = codec_registry {
        let content_type = publish.properties.get_content_type();
        let decoded = registry.decode_if_needed(&publish.payload, content_type.as_deref())?;
        publish.payload = decoded;
    }

    if let Some(ack) = ack_delivery {
        if let Some(callback) = ack.callbacks.find_one(&publish.topic_name) {
            return deliver_deferred(publish, session, flow_id, ack, callback).await;
        }
    }

    let already_delivered = match publish.qos {
        crate::QoS::AtMostOnce => false,
        crate::QoS::AtLeastOnce => {
            if let Some(packet_id) = publish.packet_id {
                ack_qos1_inbound(packet_id, writer, session, flow_id).await?;
            }
            false
        }
        crate::QoS::ExactlyOnce => {
            if let Some(packet_id) = publish.packet_id {
                let receipt = ack_qos2_inbound(packet_id, writer, session, flow_id).await?;
                receipt == Qos2Receipt::Duplicate
            } else {
                false
            }
        }
    };

    if already_delivered {
        tracing::debug!(
            packet_id = ?publish.packet_id,
            topic = %publish.topic_name,
            "Suppressing duplicate QoS2 delivery; PUBREC re-sent"
        );
        return Ok(());
    }

    publish.stream_id = flow_id.map(|f| f.raw());
    let _ = callback_manager.dispatch(&publish);

    Ok(())
}

/// Delivers an inbound message to a `subscribe_with_ack` callback, deferring the
/// acknowledgement to the token the application resolves.
///
/// The `QoS` acknowledgement is NOT written here: the dedup guard (`delivered`) is
/// set at delivery, but the PUBREC/PUBACK is emitted only when the token is resolved
/// (`DeferredAckQoS2Reconnect.tla`). A duplicate replayed after a reconnect is
/// suppressed and re-acknowledged to match the decision already recorded.
async fn deliver_deferred(
    mut publish: PublishPacket,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
    flow_id: Option<crate::transport::flow::FlowId>,
    ack: &AckDelivery<'_>,
    callback: AckPublishCallback,
) -> Result<()> {
    let qos = publish.qos;
    publish.stream_id = flow_id.map(|f| f.raw());

    if qos == QoS::AtMostOnce {
        let token = ack.dispatcher.token(0, QoS::AtMostOnce);
        ack.callbacks.dispatch(callback, publish, token);
        return Ok(());
    }

    let Some(packet_id) = publish.packet_id else {
        return Ok(());
    };

    session.read().await.register_inbound(packet_id).await?;
    if let Some(fid) = flow_id {
        session
            .read()
            .await
            .store_publish_flow(packet_id, fid)
            .await;
    }

    let first_receipt = session.write().await.mark_delivered(packet_id).await;
    if !first_receipt {
        resend_matching_ack(packet_id, qos, session, ack).await;
        tracing::debug!(
            packet_id = packet_id,
            topic = %publish.topic_name,
            "Suppressing duplicate deferred delivery; matching ack re-sent"
        );
        return Ok(());
    }

    session
        .write()
        .await
        .set_resolution(packet_id, AckResolution::Unresolved)
        .await;

    let token = ack.dispatcher.token(packet_id, qos);
    ack.callbacks.dispatch(callback, publish, token);
    Ok(())
}

/// Re-sends the success PUBREC for a duplicate PUBLISH replayed after a reconnect when
/// the application had already acked but the PUBREC was lost. Sends nothing while the
/// message is unresolved: the application still holds its token (transport reconnect), or
/// the whole in-memory state was lost with the token (process crash) and it re-delivers
/// as a first receipt. A rejected or completed exchange clears its state, so a later
/// PUBLISH on that packet id is a fresh delivery, not a duplicate seen here.
async fn resend_matching_ack(
    packet_id: u16,
    qos: QoS,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
    ack: &AckDelivery<'_>,
) {
    match session.read().await.get_resolution(packet_id).await {
        AckResolution::Acked => ack.dispatcher.enqueue(packet_id, qos, AckKind::Ack),
        AckResolution::Unresolved => {}
    }
}

async fn ack_qos1_inbound(
    packet_id: u16,
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
    flow_id: Option<crate::transport::flow::FlowId>,
) -> Result<()> {
    session
        .read()
        .await
        .flow_control()
        .read()
        .await
        .register_inbound_publish(packet_id)
        .await?;

    if let Some(fid) = flow_id {
        session
            .read()
            .await
            .store_publish_flow(packet_id, fid)
            .await;
    }

    let puback = crate::packet::puback::PubAckPacket {
        packet_id,
        reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
        properties: Properties::default(),
    };
    writer
        .lock()
        .await
        .write_packet(Packet::PubAck(puback))
        .await?;

    session
        .read()
        .await
        .flow_control()
        .read()
        .await
        .acknowledge_inbound(packet_id)
        .await;

    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Qos2Receipt {
    First,
    Duplicate,
}

async fn ack_qos2_inbound(
    packet_id: u16,
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
    flow_id: Option<crate::transport::flow::FlowId>,
) -> Result<Qos2Receipt> {
    session
        .read()
        .await
        .flow_control()
        .read()
        .await
        .register_inbound_publish(packet_id)
        .await?;

    if let Some(fid) = flow_id {
        session
            .read()
            .await
            .store_publish_flow(packet_id, fid)
            .await;
    }

    let first_receipt = session.write().await.mark_pubrec_pending(packet_id).await;

    let pubrec = crate::packet::pubrec::PubRecPacket {
        packet_id,
        reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
        properties: Properties::default(),
    };
    if let Err(e) = writer
        .lock()
        .await
        .write_packet(Packet::PubRec(pubrec))
        .await
    {
        if first_receipt {
            session.write().await.remove_pubrec(packet_id).await;
        }
        return Err(e);
    }

    if first_receipt {
        Ok(Qos2Receipt::First)
    } else {
        Ok(Qos2Receipt::Duplicate)
    }
}

#[cfg(feature = "transport-quic")]
pub(super) async fn handle_incoming_packet_no_writer(
    packet: Packet,
    callback_manager: &Arc<CallbackManager>,
    flow_id: Option<FlowId>,
    keepalive_state: &Arc<Mutex<KeepaliveState>>,
    codec_registry: Option<&Arc<CodecRegistry>>,
) -> Result<()> {
    match packet {
        Packet::Publish(mut publish) => {
            if publish.qos != crate::QoS::AtMostOnce {
                return Err(MqttError::ProtocolError(
                    "QoS > 0 publish received on unidirectional stream".to_string(),
                ));
            }
            if let Some(registry) = codec_registry {
                let content_type = publish.properties.get_content_type();
                let decoded =
                    registry.decode_if_needed(&publish.payload, content_type.as_deref())?;
                publish.payload = decoded;
            }
            publish.stream_id = flow_id.map(|f| f.raw());
            let _ = callback_manager.dispatch(&publish);
            Ok(())
        }
        Packet::PingResp => {
            keepalive_state.lock().record_pong_received();
            Ok(())
        }
        Packet::Disconnect(disconnect) => {
            tracing::info!("Server sent DISCONNECT: {:?}", disconnect.reason_code);
            Err(MqttError::ConnectionError(
                "Server disconnected".to_string(),
            ))
        }
        _ => Ok(()),
    }
}

pub(super) async fn handle_pubrec_outgoing(
    pubrec: crate::packet::pubrec::PubRecPacket,
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
) -> Result<()> {
    session
        .write()
        .await
        .complete_pubrec(pubrec.packet_id)
        .await;

    let pub_rel = crate::packet::pubrel::PubRelPacket {
        packet_id: pubrec.packet_id,
        reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
        properties: Properties::default(),
    };

    writer
        .lock()
        .await
        .write_packet(crate::packet::Packet::PubRel(pub_rel))
        .await?;

    session.write().await.store_pubrel(pubrec.packet_id).await;

    Ok(())
}

pub(super) async fn handle_pubcomp_outgoing(
    pubcomp: crate::packet::pubcomp::PubCompPacket,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
) -> Result<()> {
    session
        .write()
        .await
        .complete_pubrel(pubcomp.packet_id)
        .await;

    super::DirectClientInner::release_outbound_quota(session, Some(pubcomp.packet_id)).await;

    Ok(())
}

pub(super) async fn handle_pubrel(
    pubrel: crate::packet::pubrel::PubRelPacket,
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
) -> Result<()> {
    session
        .write()
        .await
        .clear_inbound_state(pubrel.packet_id)
        .await;

    let pubcomp = crate::packet::pubcomp::PubCompPacket {
        packet_id: pubrel.packet_id,
        reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
        properties: Properties::default(),
    };

    writer
        .lock()
        .await
        .write_packet(Packet::PubComp(pubcomp))
        .await?;

    session
        .read()
        .await
        .flow_control()
        .read()
        .await
        .acknowledge_inbound(pubrel.packet_id)
        .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{handle_publish_with_ack, UnifiedWriter};
    use crate::callback::CallbackManager;
    use crate::packet::publish::PublishPacket;
    use crate::session::{SessionConfig, SessionState};
    use crate::QoS;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc;

    const PUBREC_HEADER: u8 = 0x50;

    async fn loopback_writer() -> (Arc<tokio::sync::Mutex<UnifiedWriter>>, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, accepted) = tokio::join!(TcpStream::connect(addr), listener.accept());
        let (_read_half, write_half) = client.unwrap().into_split();
        (
            Arc::new(tokio::sync::Mutex::new(UnifiedWriter::Tcp(write_half))),
            accepted.unwrap().0,
        )
    }

    async fn read_packet_header(server: &mut TcpStream) -> u8 {
        let mut fixed = [0u8; 2];
        tokio::time::timeout(Duration::from_secs(2), server.read_exact(&mut fixed))
            .await
            .expect("timed out waiting for a packet")
            .expect("connection closed before a packet arrived");
        let mut body = vec![0u8; fixed[1] as usize];
        server.read_exact(&mut body).await.unwrap();
        fixed[0]
    }

    fn session() -> Arc<tokio::sync::RwLock<SessionState>> {
        Arc::new(tokio::sync::RwLock::new(SessionState::new(
            "test-client".to_string(),
            SessionConfig::default(),
            true,
        )))
    }

    fn qos2_publish(packet_id: u16, topic: &str) -> PublishPacket {
        let mut publish = PublishPacket::new(topic, &b"payload"[..], QoS::ExactlyOnce);
        publish.packet_id = Some(packet_id);
        publish
    }

    fn counting_callbacks(topic: &str) -> (Arc<CallbackManager>, mpsc::UnboundedReceiver<String>) {
        let callbacks = Arc::new(CallbackManager::new());
        let (tx, rx) = mpsc::unbounded_channel();
        callbacks
            .register(
                topic,
                Arc::new(move |packet: PublishPacket| {
                    let _ = tx.send(packet.topic_name);
                }),
            )
            .unwrap();
        (callbacks, rx)
    }

    async fn delivery_count(rx: &mut mpsc::UnboundedReceiver<String>) -> usize {
        let mut count = 0;
        while tokio::time::timeout(Duration::from_millis(200), rx.recv())
            .await
            .ok()
            .flatten()
            .is_some()
        {
            count += 1;
        }
        count
    }

    #[tokio::test]
    async fn duplicate_qos2_publish_is_delivered_once_and_pubrec_resent() {
        let topic = "sensors/temp";
        let (writer, mut server) = loopback_writer().await;
        let session = session();
        let (callbacks, mut rx) = counting_callbacks(topic);

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &callbacks,
            None,
            None,
            None,
        )
        .await
        .expect("first QoS2 publish must be handled");

        let mut duplicate = qos2_publish(1, topic);
        duplicate.dup = true;
        handle_publish_with_ack(duplicate, &writer, &session, &callbacks, None, None, None)
            .await
            .expect("duplicate QoS2 publish must be handled, not error");

        assert_eq!(
            delivery_count(&mut rx).await,
            1,
            "a redelivered QoS2 packet_id must reach the application exactly once"
        );

        assert_eq!(read_packet_header(&mut server).await, PUBREC_HEADER);
        assert_eq!(
            read_packet_header(&mut server).await,
            PUBREC_HEADER,
            "PUBREC must be re-sent for the duplicate so the handshake still completes"
        );
    }

    #[tokio::test]
    async fn distinct_qos2_packet_ids_are_each_delivered() {
        let topic = "sensors/temp";
        let (writer, _server) = loopback_writer().await;
        let session = session();
        let (callbacks, mut rx) = counting_callbacks(topic);

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &callbacks,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        handle_publish_with_ack(
            qos2_publish(2, topic),
            &writer,
            &session,
            &callbacks,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            delivery_count(&mut rx).await,
            2,
            "the duplicate guard must not suppress distinct packet ids"
        );
    }

    #[tokio::test]
    async fn concurrent_duplicate_qos2_publishes_are_delivered_once() {
        let topic = "sensors/temp";
        let (writer_a, _server_a) = loopback_writer().await;
        let (writer_b, _server_b) = loopback_writer().await;
        let session = session();
        let (callbacks, mut rx) = counting_callbacks(topic);

        let a = {
            let (session, callbacks) = (Arc::clone(&session), Arc::clone(&callbacks));
            tokio::spawn(async move {
                handle_publish_with_ack(
                    qos2_publish(1, topic),
                    &writer_a,
                    &session,
                    &callbacks,
                    None,
                    None,
                    None,
                )
                .await
            })
        };
        let b = {
            let (session, callbacks) = (Arc::clone(&session), Arc::clone(&callbacks));
            tokio::spawn(async move {
                handle_publish_with_ack(
                    qos2_publish(1, topic),
                    &writer_b,
                    &session,
                    &callbacks,
                    None,
                    None,
                    None,
                )
                .await
            })
        };
        a.await.unwrap().unwrap();
        b.await.unwrap().unwrap();

        assert_eq!(
            delivery_count(&mut rx).await,
            1,
            "QUIC spawns a task per stream sharing one session; a racing duplicate must still \
             reach the application exactly once"
        );
    }

    #[tokio::test]
    async fn outbound_pubrel_does_not_mask_an_inbound_packet_id() {
        let topic = "sensors/temp";
        let (writer, _server) = loopback_writer().await;
        let session = session();
        let (callbacks, mut rx) = counting_callbacks(topic);

        session.write().await.store_pubrel(1).await;

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &callbacks,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            delivery_count(&mut rx).await,
            1,
            "inbound and outbound packet ids are independent namespaces; an outbound PUBREL for \
             id 1 must not make an inbound PUBLISH id 1 look like a duplicate"
        );
    }

    #[tokio::test]
    async fn qos2_packet_id_is_deliverable_again_after_the_handshake_completes() {
        let topic = "sensors/temp";
        let (writer, _server) = loopback_writer().await;
        let session = session();
        let (callbacks, mut rx) = counting_callbacks(topic);

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &callbacks,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        session.write().await.remove_pubrec(1).await;
        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &callbacks,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            delivery_count(&mut rx).await,
            2,
            "once PUBCOMP completes the packet id is released and a reuse is a new message"
        );
    }

    use super::AckDelivery;
    use crate::client::direct::ack::{AckCallbackManager, AckDispatcher, AckToken};

    async fn expect_no_packet(server: &mut TcpStream) {
        let mut buf = [0u8; 1];
        match tokio::time::timeout(Duration::from_millis(300), server.read(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => {
                panic!("a packet was written before the token was resolved")
            }
            _ => {}
        }
    }

    fn ack_setup(
        topic: &str,
        session: &Arc<tokio::sync::RwLock<SessionState>>,
    ) -> (
        Arc<AckCallbackManager>,
        Arc<AckDispatcher>,
        mpsc::UnboundedReceiver<AckToken>,
    ) {
        let dispatcher = Arc::new(AckDispatcher::new(Arc::clone(session)));
        let callbacks = Arc::new(AckCallbackManager::new());
        let (tx, rx) = mpsc::unbounded_channel();
        callbacks.register(
            topic,
            Arc::new(move |_publish: PublishPacket, token: AckToken| {
                let _ = tx.send(token);
            }),
        );
        (callbacks, dispatcher, rx)
    }

    #[tokio::test]
    async fn deferred_qos2_withholds_pubrec_until_the_token_is_acked() {
        let topic = "sensors/temp";
        let (writer, mut server) = loopback_writer().await;
        let session = session();
        let plain = Arc::new(CallbackManager::new());
        let (callbacks, dispatcher, mut rx) = ack_setup(topic, &session);
        dispatcher.set_writer(Arc::clone(&writer)).await;
        let ack = AckDelivery {
            callbacks: &callbacks,
            dispatcher: &dispatcher,
        };

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &plain,
            None,
            None,
            Some(&ack),
        )
        .await
        .expect("deferred QoS2 publish must be handled");

        let token = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("callback must receive a token")
            .expect("token channel open");
        assert_eq!(token.packet_id(), 1);
        assert_eq!(token.qos(), QoS::ExactlyOnce);

        expect_no_packet(&mut server).await;

        token.ack();
        assert_eq!(
            read_packet_header(&mut server).await,
            PUBREC_HEADER,
            "PUBREC must be written only after the token is acked"
        );
        assert!(session.read().await.has_pubrec(1).await);
    }

    #[tokio::test]
    async fn deferred_qos2_token_drop_emits_a_reason_coded_pubrec() {
        let topic = "sensors/temp";
        let (writer, mut server) = loopback_writer().await;
        let session = session();
        let plain = Arc::new(CallbackManager::new());
        let (callbacks, dispatcher, mut rx) = ack_setup(topic, &session);
        dispatcher.set_writer(Arc::clone(&writer)).await;
        let ack = AckDelivery {
            callbacks: &callbacks,
            dispatcher: &dispatcher,
        };

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &plain,
            None,
            None,
            Some(&ack),
        )
        .await
        .unwrap();

        let token = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        expect_no_packet(&mut server).await;
        drop(token);
        assert_eq!(
            read_packet_header(&mut server).await,
            PUBREC_HEADER,
            "dropping a token must emit a (reason-coded) PUBREC so the handshake never wedges"
        );
    }

    const PUBCOMP_HEADER: u8 = 0x70;

    async fn wait_for<F, Fut>(mut cond: F)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        for _ in 0..100 {
            if cond().await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition never became true");
    }

    /// After a deferred `QoS` 2 handshake completes (PUBREL -> PUBCOMP), the broker may reuse
    /// the packet id for a new message, which must be delivered, not suppressed as a duplicate.
    #[tokio::test]
    async fn deferred_qos2_reused_packet_id_after_completion_is_delivered_again() {
        let topic = "sensors/temp";
        let (writer, mut server) = loopback_writer().await;
        let session = session();
        let plain = Arc::new(CallbackManager::new());
        let (callbacks, dispatcher, mut rx) = ack_setup(topic, &session);
        dispatcher.set_writer(Arc::clone(&writer)).await;
        let ack = AckDelivery {
            callbacks: &callbacks,
            dispatcher: &dispatcher,
        };

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &plain,
            None,
            None,
            Some(&ack),
        )
        .await
        .unwrap();
        let token1 = rx.recv().await.expect("first delivery");
        token1.ack();
        assert_eq!(read_packet_header(&mut server).await, PUBREC_HEADER);
        wait_for(|| async { session.read().await.has_pubrec(1).await }).await;

        super::handle_pubrel(
            crate::packet::pubrel::PubRelPacket::new(1),
            &writer,
            &session,
        )
        .await
        .expect("PUBREL completes the handshake");
        assert_eq!(read_packet_header(&mut server).await, PUBCOMP_HEADER);

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &plain,
            None,
            None,
            Some(&ack),
        )
        .await
        .unwrap();

        let token2 = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
        assert!(
            matches!(token2, Ok(Some(_))),
            "a message reusing a packet id whose handshake already completed must be \
             delivered, not suppressed as a duplicate"
        );
    }

    /// After a deferred `QoS` 2 message is rejected (terminal error PUBREC, no PUBREL), the broker
    /// releases and may reuse the id; a new message on that id must be delivered, not re-rejected.
    #[tokio::test]
    async fn deferred_qos2_reused_packet_id_after_reject_is_delivered_again() {
        let topic = "sensors/temp";
        let (writer, mut server) = loopback_writer().await;
        let session = session();
        let plain = Arc::new(CallbackManager::new());
        let (callbacks, dispatcher, mut rx) = ack_setup(topic, &session);
        dispatcher.set_writer(Arc::clone(&writer)).await;
        let ack = AckDelivery {
            callbacks: &callbacks,
            dispatcher: &dispatcher,
        };

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &plain,
            None,
            None,
            Some(&ack),
        )
        .await
        .unwrap();
        let token1 = rx.recv().await.expect("first delivery");
        token1.reject(crate::protocol::v5::reason_codes::ReasonCode::UnspecifiedError);
        assert_eq!(read_packet_header(&mut server).await, PUBREC_HEADER);
        wait_for(|| async { !session.read().await.is_delivered(1).await }).await;

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &plain,
            None,
            None,
            Some(&ack),
        )
        .await
        .unwrap();

        let token2 = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
        assert!(
            matches!(token2, Ok(Some(_))),
            "a message reusing a packet id whose exchange was rejected must be delivered, \
             not suppressed and re-rejected"
        );
    }

    /// A PUBREL clears the inbound dedup guard even when no PUBREC was recorded (a spurious or
    /// early PUBREL); a lingering guard would suppress a later reused packet id.
    #[tokio::test]
    async fn deferred_qos2_pubrel_clears_dedup_without_pubrec() {
        let (writer, mut server) = loopback_writer().await;
        let session = session();

        assert!(session.read().await.mark_delivered(1).await);
        assert!(!session.read().await.has_pubrec(1).await);

        super::handle_pubrel(
            crate::packet::pubrel::PubRelPacket::new(1),
            &writer,
            &session,
        )
        .await
        .expect("PUBREL is answered with PUBCOMP");
        assert_eq!(read_packet_header(&mut server).await, PUBCOMP_HEADER);

        assert!(
            !session.read().await.is_delivered(1).await,
            "a PUBREL must clear the dedup guard even without a recorded PUBREC"
        );
    }

    /// `reject()` with a non-error reason (e.g. `Success`) is normalized to an error and still
    /// takes the reject path: it clears the dedup state rather than behaving like ack.
    #[tokio::test]
    async fn deferred_qos2_reject_with_success_reason_still_clears_state() {
        let topic = "sensors/temp";
        let (writer, mut server) = loopback_writer().await;
        let session = session();
        let plain = Arc::new(CallbackManager::new());
        let (callbacks, dispatcher, mut rx) = ack_setup(topic, &session);
        dispatcher.set_writer(Arc::clone(&writer)).await;
        let ack = AckDelivery {
            callbacks: &callbacks,
            dispatcher: &dispatcher,
        };

        handle_publish_with_ack(
            qos2_publish(1, topic),
            &writer,
            &session,
            &plain,
            None,
            None,
            Some(&ack),
        )
        .await
        .unwrap();
        let token1 = rx.recv().await.expect("first delivery");
        token1.reject(crate::protocol::v5::reason_codes::ReasonCode::Success);
        assert_eq!(read_packet_header(&mut server).await, PUBREC_HEADER);
        wait_for(|| async { !session.read().await.is_delivered(1).await }).await;
        assert_eq!(
            session.read().await.get_resolution(1).await,
            crate::session::state::AckResolution::Unresolved,
            "reject(Success) must clear state (reject path), not record an Acked resolution"
        );
    }
}
