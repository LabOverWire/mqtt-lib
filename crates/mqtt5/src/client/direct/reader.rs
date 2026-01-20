//! Packet reading loop and QUIC stream handling

use crate::callback::CallbackManager;
use crate::client::auth_handler::{AuthHandler, AuthResponse};
use crate::codec::CodecRegistry;
use crate::error::{MqttError, Result};
use crate::packet::auth::AuthPacket;
use crate::packet::suback::SubAckPacket;
use crate::packet::unsuback::UnsubAckPacket;
use crate::packet::Packet;
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::session::SessionState;
use crate::transport::flow::{
    FlowFlags, FlowHeader, FlowId, FLOW_TYPE_CLIENT_DATA, FLOW_TYPE_CONTROL, FLOW_TYPE_SERVER_DATA,
};
use crate::transport::{PacketReader, PacketWriter};
use bytes::Bytes;
use parking_lot::Mutex;
use quinn::Connection;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::sync::oneshot;

use super::handlers::handle_incoming_packet_with_writer;
use super::keepalive::KeepaliveState;
use super::unified::{UnifiedReader, UnifiedWriter};

#[derive(Clone)]
pub(super) struct PacketReaderContext {
    pub(super) session: Arc<tokio::sync::RwLock<SessionState>>,
    pub(super) callback_manager: Arc<CallbackManager>,
    pub(super) suback_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<SubAckPacket>>>>,
    pub(super) unsuback_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<UnsubAckPacket>>>>,
    pub(super) puback_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<ReasonCode>>>>,
    pub(super) pubcomp_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<ReasonCode>>>>,
    pub(super) writer: Arc<tokio::sync::Mutex<UnifiedWriter>>,
    pub(super) connected: Arc<AtomicBool>,
    pub(super) protocol_version: u8,
    pub(super) auth_handler: Option<Arc<dyn AuthHandler>>,
    pub(super) auth_method: Option<String>,
    pub(super) keepalive_state: Arc<Mutex<KeepaliveState>>,
    pub(super) codec_registry: Option<Arc<CodecRegistry>>,
}

pub(super) async fn packet_reader_task_with_responses(
    mut reader: UnifiedReader,
    ctx: PacketReaderContext,
) {
    tracing::debug!("Packet reader task started and ready to process incoming packets");
    loop {
        let packet = reader.read_packet().await;

        match packet {
            Ok(packet) => {
                tracing::trace!("Received packet: {:?}", packet);
                match &packet {
                    Packet::SubAck(suback) => {
                        if let Some(tx) = ctx.suback_channels.lock().remove(&suback.packet_id) {
                            let _ = tx.send(suback.clone());
                            continue;
                        }
                    }
                    Packet::UnsubAck(unsuback) => {
                        if let Some(tx) = ctx.unsuback_channels.lock().remove(&unsuback.packet_id) {
                            let _ = tx.send(unsuback.clone());
                            continue;
                        }
                    }
                    Packet::PubAck(puback) => {
                        if let Some(tx) = ctx.puback_channels.lock().remove(&puback.packet_id) {
                            let _ = tx.send(puback.reason_code);
                            continue;
                        }
                    }
                    Packet::PubRec(pubrec) => {
                        if pubrec.reason_code.is_error() {
                            tracing::debug!(
                                packet_id = pubrec.packet_id,
                                reason_code = ?pubrec.reason_code,
                                "QoS 2 PUBREC rejected"
                            );
                            if let Some(tx) = ctx.pubcomp_channels.lock().remove(&pubrec.packet_id)
                            {
                                let _ = tx.send(pubrec.reason_code);
                            }
                            ctx.session
                                .write()
                                .await
                                .remove_unacked_publish(pubrec.packet_id)
                                .await;
                            continue;
                        }
                    }
                    Packet::PubComp(pubcomp) => {
                        if let Some(tx) = ctx.pubcomp_channels.lock().remove(&pubcomp.packet_id) {
                            let _ = tx.send(pubcomp.reason_code);
                        }
                    }
                    Packet::Auth(ref auth) => {
                        if let Err(e) = handle_auth_packet(auth.clone(), &ctx).await {
                            tracing::error!("Error handling AUTH packet: {e}");
                            ctx.connected.store(false, Ordering::SeqCst);
                            break;
                        }
                        continue;
                    }
                    _ => {}
                }

                if let Err(e) = handle_incoming_packet_with_writer(
                    packet,
                    &ctx.writer,
                    &ctx.session,
                    &ctx.callback_manager,
                    None,
                    &ctx.keepalive_state,
                    ctx.codec_registry.as_ref(),
                )
                .await
                {
                    tracing::error!("Error handling packet: {e}");
                    ctx.connected.store(false, Ordering::SeqCst);
                    break;
                }
            }
            Err(e) => {
                tracing::error!("Error reading packet: {e}");
                ctx.connected.store(false, Ordering::SeqCst);
                break;
            }
        }
    }

    ctx.connected.store(false, Ordering::SeqCst);

    ctx.puback_channels.lock().drain();
    ctx.pubcomp_channels.lock().drain();
    ctx.suback_channels.lock().drain();
    ctx.unsuback_channels.lock().drain();
}

async fn handle_auth_packet(auth: AuthPacket, ctx: &PacketReaderContext) -> Result<()> {
    tracing::debug!(
        "CLIENT: Received AUTH during session with reason: {:?}",
        auth.reason_code
    );

    match auth.reason_code {
        ReasonCode::ContinueAuthentication => {
            let handler = ctx
                .auth_handler
                .as_ref()
                .ok_or(MqttError::AuthenticationFailed)?;

            let auth_method = auth.authentication_method().unwrap_or("");
            let auth_data = auth.authentication_data();

            let response = handler.handle_challenge(auth_method, auth_data).await?;

            match response {
                AuthResponse::Continue(data) => {
                    let method = ctx.auth_method.clone().unwrap_or_default();
                    let auth_packet = AuthPacket::continue_authentication(method, Some(data))?;
                    ctx.writer
                        .lock()
                        .await
                        .write_packet(Packet::Auth(auth_packet))
                        .await?;
                }
                AuthResponse::Success => {
                    tracing::debug!("CLIENT: Auth handler indicated success for re-auth challenge");
                }
                AuthResponse::Abort(reason) => {
                    tracing::warn!("CLIENT: Re-auth aborted: {}", reason);
                    return Err(MqttError::AuthenticationFailed);
                }
            }
        }
        ReasonCode::Success => {
            tracing::info!("CLIENT: Re-authentication completed successfully");
        }
        _ => {
            tracing::warn!(
                "CLIENT: Re-authentication failed with reason: {:?}",
                auth.reason_code
            );
            return Err(MqttError::AuthenticationFailed);
        }
    }

    Ok(())
}

pub(super) async fn quic_stream_acceptor_task(
    connection: Arc<Connection>,
    ctx: PacketReaderContext,
) {
    loop {
        match connection.accept_bi().await {
            Ok((send, recv)) => {
                tracing::debug!("Accepted new QUIC stream");
                let ctx_for_reader = ctx.clone();
                tokio::spawn(async move {
                    quic_stream_reader_task(recv, send, ctx_for_reader).await;
                });
            }
            Err(e) => {
                tracing::error!("Error accepting QUIC stream: {e}");
                ctx.connected.store(false, Ordering::SeqCst);
                break;
            }
        }
    }
}

fn is_flow_header_byte(b: u8) -> bool {
    matches!(
        b,
        FLOW_TYPE_CONTROL | FLOW_TYPE_CLIENT_DATA | FLOW_TYPE_SERVER_DATA
    )
}

async fn try_read_server_flow_header(
    recv: &mut quinn::RecvStream,
) -> Result<Option<(FlowId, FlowFlags, Option<StdDuration>)>> {
    let chunk = recv
        .read_chunk(1, true)
        .await
        .map_err(|e| MqttError::ConnectionError(format!("Failed to peek stream: {e}")))?;

    let Some(chunk) = chunk else {
        return Ok(None);
    };

    if chunk.bytes.is_empty() {
        return Ok(None);
    }

    let first_byte = chunk.bytes[0];
    if !is_flow_header_byte(first_byte) {
        return Ok(None);
    }

    let mut header_buf = Vec::with_capacity(32);
    header_buf.extend_from_slice(&chunk.bytes);

    while header_buf.len() < 32 {
        match recv.read_chunk(32 - header_buf.len(), true).await {
            Ok(Some(chunk)) if !chunk.bytes.is_empty() => {
                header_buf.extend_from_slice(&chunk.bytes);
            }
            Ok(_) => break,
            Err(e) => {
                return Err(MqttError::ConnectionError(format!(
                    "Failed to read flow header: {e}"
                )));
            }
        }
    }

    let mut bytes = Bytes::from(header_buf);
    let flow_header = FlowHeader::decode(&mut bytes)?;

    match flow_header {
        FlowHeader::Control(h) => {
            tracing::trace!(flow_id = ?h.flow_id, "Parsed control flow header from server");
            Ok(Some((h.flow_id, h.flags, None)))
        }
        FlowHeader::ClientData(h) | FlowHeader::ServerData(h) => {
            let expire = if h.expire_interval > 0 {
                Some(StdDuration::from_secs(h.expire_interval))
            } else {
                None
            };
            tracing::debug!(flow_id = ?h.flow_id, is_server = h.is_server_flow(), expire = ?expire, "Parsed data flow header from server");
            Ok(Some((h.flow_id, h.flags, expire)))
        }
        FlowHeader::UserDefined(_) => {
            tracing::trace!("Ignoring user-defined flow header");
            Ok(None)
        }
    }
}

async fn quic_stream_reader_task(
    mut recv: quinn::RecvStream,
    send: quinn::SendStream,
    ctx: PacketReaderContext,
) {
    let flow_id = match try_read_server_flow_header(&mut recv).await {
        Ok(Some((id, flags, expire))) => {
            tracing::debug!(
                flow_id = ?id,
                is_server_initiated = id.is_server_initiated(),
                ?flags,
                ?expire,
                "Server-initiated stream with flow header"
            );
            Some(id)
        }
        Ok(None) => {
            tracing::trace!("No flow header on server-initiated stream");
            None
        }
        Err(e) => {
            tracing::warn!("Error parsing server flow header: {e}");
            None
        }
    };

    let stream_writer = Arc::new(tokio::sync::Mutex::new(UnifiedWriter::Quic(send)));

    loop {
        match recv.read_packet(ctx.protocol_version).await {
            Ok(packet) => {
                tracing::trace!(flow_id = ?flow_id, "Received packet on server-initiated QUIC stream: {:?}", packet);
                if let Err(e) = handle_incoming_packet_with_writer(
                    packet,
                    &stream_writer,
                    &ctx.session,
                    &ctx.callback_manager,
                    flow_id,
                    &ctx.keepalive_state,
                    ctx.codec_registry.as_ref(),
                )
                .await
                {
                    tracing::error!(flow_id = ?flow_id, "Error handling packet from server stream: {e}");
                    break;
                }
            }
            Err(e) => {
                tracing::debug!(flow_id = ?flow_id, "Server-initiated QUIC stream closed or error: {e}");
                break;
            }
        }
    }
}
