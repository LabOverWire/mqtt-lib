//! Packet type handlers for incoming packets

use crate::callback::CallbackManager;
use crate::codec::CodecRegistry;
use crate::error::{MqttError, Result};
use crate::packet::Packet;
use crate::protocol::v5::properties::Properties;
use crate::session::SessionState;
use crate::transport::PacketWriter;
use parking_lot::Mutex;
use std::sync::Arc;

use super::keepalive::KeepaliveState;
use super::unified::UnifiedWriter;
#[cfg(feature = "transport-quic")]
use crate::transport::flow::FlowId;

pub(super) async fn handle_incoming_packet_with_writer(
    packet: Packet,
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
    callback_manager: &Arc<CallbackManager>,
    flow_id: Option<crate::transport::flow::FlowId>,
    keepalive_state: &Arc<Mutex<KeepaliveState>>,
    codec_registry: Option<&Arc<CodecRegistry>>,
) -> Result<()> {
    match packet {
        Packet::Publish(publish) => {
            handle_publish_with_ack(
                publish,
                writer,
                session,
                callback_manager,
                flow_id,
                codec_registry,
            )
            .await
        }
        Packet::PingResp => {
            keepalive_state.lock().record_pong_received();
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
) -> Result<()> {
    if let Some(registry) = codec_registry {
        let content_type = publish.properties.get_content_type();
        let decoded = registry.decode_if_needed(&publish.payload, content_type.as_deref())?;
        publish.payload = decoded;
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

    Ok(())
}

pub(super) async fn handle_pubrel(
    pubrel: crate::packet::pubrel::PubRelPacket,
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    session: &Arc<tokio::sync::RwLock<SessionState>>,
) -> Result<()> {
    let has_pubrec = session.read().await.has_pubrec(pubrel.packet_id).await;

    if has_pubrec {
        session.write().await.remove_pubrec(pubrel.packet_id).await;

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
    } else {
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
    }

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
        )
        .await
        .expect("first QoS2 publish must be handled");

        let mut duplicate = qos2_publish(1, topic);
        duplicate.dup = true;
        handle_publish_with_ack(duplicate, &writer, &session, &callbacks, None, None)
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
        )
        .await
        .unwrap();

        assert_eq!(
            delivery_count(&mut rx).await,
            2,
            "once PUBCOMP completes the packet id is released and a reuse is a new message"
        );
    }
}
