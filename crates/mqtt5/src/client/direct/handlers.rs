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

    match publish.qos {
        crate::QoS::AtMostOnce => {}
        crate::QoS::AtLeastOnce => {
            if let Some(packet_id) = publish.packet_id {
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
            }
        }
        crate::QoS::ExactlyOnce => {
            if let Some(packet_id) = publish.packet_id {
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
                session
                    .write()
                    .await
                    .store_unacked_publish(publish.clone())
                    .await?;
                let pubrec = crate::packet::pubrec::PubRecPacket {
                    packet_id,
                    reason_code: crate::protocol::v5::reason_codes::ReasonCode::Success,
                    properties: Properties::default(),
                };
                writer
                    .lock()
                    .await
                    .write_packet(Packet::PubRec(pubrec))
                    .await?;
                session.write().await.store_pubrec(packet_id).await;
            }
        }
    }

    let _ = callback_manager.dispatch(&publish);

    Ok(())
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
