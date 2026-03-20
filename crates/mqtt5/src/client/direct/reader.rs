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
use crate::transport::PacketWriter;
use bytes::{Buf, Bytes, BytesMut};
use parking_lot::Mutex;
use quinn::Connection;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration as StdDuration;
use tokio::sync::oneshot;

use super::handlers::{handle_incoming_packet_no_writer, handle_incoming_packet_with_writer};
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
        tokio::select! {
            result = connection.accept_uni() => {
                match result {
                    Ok(recv) => {
                        tracing::debug!("Accepted unidirectional QUIC stream");
                        let ctx_for_reader = ctx.clone();
                        tokio::spawn(async move {
                            quic_uni_stream_reader_task(recv, ctx_for_reader).await;
                        });
                    }
                    Err(e) => {
                        let reason = crate::transport::quic_error::parse_connection_error(&e);
                        tracing::error!("QUIC uni stream accept ended: {reason}");
                        ctx.connected.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            }
            result = connection.accept_bi() => {
                match result {
                    Ok((send, recv)) => {
                        tracing::debug!("Accepted bidirectional QUIC stream");
                        let ctx_for_reader = ctx.clone();
                        tokio::spawn(async move {
                            quic_stream_reader_task(recv, send, ctx_for_reader).await;
                        });
                    }
                    Err(e) => {
                        let reason = crate::transport::quic_error::parse_connection_error(&e);
                        tracing::error!("QUIC bi stream accept ended: {reason}");
                        ctx.connected.store(false, Ordering::SeqCst);
                        break;
                    }
                }
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

struct ServerFlowResult {
    flow_id: Option<FlowId>,
    flags: Option<FlowFlags>,
    expire: Option<StdDuration>,
    leftover: BytesMut,
}

async fn try_read_server_flow_header(recv: &mut quinn::RecvStream) -> Result<ServerFlowResult> {
    let chunk = recv
        .read_chunk(1, true)
        .await
        .map_err(|e| MqttError::ConnectionError(format!("Failed to peek stream: {e}")))?;

    let Some(chunk) = chunk else {
        return Ok(ServerFlowResult {
            flow_id: None,
            flags: None,
            expire: None,
            leftover: BytesMut::new(),
        });
    };

    if chunk.bytes.is_empty() {
        return Ok(ServerFlowResult {
            flow_id: None,
            flags: None,
            expire: None,
            leftover: BytesMut::new(),
        });
    }

    let first_byte = chunk.bytes[0];
    if !is_flow_header_byte(first_byte) {
        let mut leftover = BytesMut::with_capacity(chunk.bytes.len());
        leftover.extend_from_slice(&chunk.bytes);
        return Ok(ServerFlowResult {
            flow_id: None,
            flags: None,
            expire: None,
            leftover,
        });
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

    let mut leftover = BytesMut::with_capacity(bytes.remaining());
    if bytes.has_remaining() {
        leftover.extend_from_slice(&bytes);
    }

    match flow_header {
        FlowHeader::Control(h) => {
            tracing::trace!(flow_id = ?h.flow_id, "Parsed control flow header from server");
            Ok(ServerFlowResult {
                flow_id: Some(h.flow_id),
                flags: Some(h.flags),
                expire: None,
                leftover,
            })
        }
        FlowHeader::ClientData(h) | FlowHeader::ServerData(h) => {
            let expire = if h.expire_interval > 0 {
                Some(StdDuration::from_secs(h.expire_interval))
            } else {
                None
            };
            tracing::debug!(flow_id = ?h.flow_id, is_server = h.is_server_flow(), expire = ?expire, "Parsed data flow header from server");
            Ok(ServerFlowResult {
                flow_id: Some(h.flow_id),
                flags: Some(h.flags),
                expire,
                leftover,
            })
        }
        FlowHeader::UserDefined(_) => {
            tracing::trace!("Ignoring user-defined flow header");
            Ok(ServerFlowResult {
                flow_id: None,
                flags: None,
                expire: None,
                leftover,
            })
        }
    }
}

async fn read_packet_with_buffer(
    recv: &mut quinn::RecvStream,
    buffer: &mut BytesMut,
    protocol_version: u8,
) -> Result<Packet> {
    use crate::packet::FixedHeader;

    while buffer.len() < 2 {
        let mut tmp = [0u8; 64];
        let n = recv
            .read(&mut tmp)
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
            .ok_or(MqttError::ClientClosed)?;
        if n == 0 {
            return Err(MqttError::ClientClosed);
        }
        buffer.extend_from_slice(&tmp[..n]);
    }

    let mut remaining_length = 0u32;
    let mut multiplier = 1u32;
    let mut remaining_length_bytes = 1usize;

    for i in 1..5 {
        if i >= buffer.len() {
            let mut tmp = [0u8; 64];
            let n = recv
                .read(&mut tmp)
                .await
                .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
                .ok_or(MqttError::ClientClosed)?;
            if n == 0 {
                return Err(MqttError::ClientClosed);
            }
            buffer.extend_from_slice(&tmp[..n]);
        }

        let byte = buffer[i];
        remaining_length += u32::from(byte & 0x7F) * multiplier;
        multiplier *= 128;
        remaining_length_bytes = i;

        if (byte & 0x80) == 0 {
            break;
        }

        if i == 4 {
            return Err(MqttError::MalformedPacket(
                "Invalid remaining length encoding".to_string(),
            ));
        }
    }

    let header_len = 1 + remaining_length_bytes;
    let total_len = header_len + remaining_length as usize;

    while buffer.len() < total_len {
        let needed = total_len - buffer.len();
        let chunk_size = needed.min(4096);
        let old_len = buffer.len();
        buffer.resize(old_len + chunk_size, 0);
        let n = recv
            .read(&mut buffer[old_len..old_len + chunk_size])
            .await
            .map_err(|e| MqttError::ConnectionError(format!("QUIC read error: {e}")))?
            .ok_or(MqttError::ClientClosed)?;
        if n == 0 {
            return Err(MqttError::ClientClosed);
        }
        buffer.truncate(old_len + n);
    }

    let packet_bytes = buffer.split_to(total_len);
    let mut header_buf = packet_bytes.clone().freeze();
    let fixed_header = FixedHeader::decode(&mut header_buf)?;

    let mut payload_buf = BytesMut::from(&packet_bytes[header_len..]);
    Packet::decode_from_body_with_version(
        fixed_header.packet_type,
        &fixed_header,
        &mut payload_buf,
        protocol_version,
    )
}

async fn quic_stream_reader_task(
    mut recv: quinn::RecvStream,
    send: quinn::SendStream,
    ctx: PacketReaderContext,
) {
    let (flow_id, mut buffer) = match try_read_server_flow_header(&mut recv).await {
        Ok(result) => {
            let flow_id = if let (Some(id), Some(flags)) = (result.flow_id, result.flags) {
                tracing::debug!(
                    flow_id = ?id,
                    is_server_initiated = id.is_server_initiated(),
                    ?flags,
                    expire = ?result.expire,
                    "Server-initiated stream with flow header"
                );
                Some(id)
            } else {
                tracing::trace!("No flow header on server-initiated stream");
                None
            };
            (flow_id, result.leftover)
        }
        Err(e) => {
            tracing::warn!("Error parsing server flow header: {e}");
            (None, BytesMut::new())
        }
    };

    let stream_writer = Arc::new(tokio::sync::Mutex::new(UnifiedWriter::Quic(send)));

    loop {
        match read_packet_with_buffer(&mut recv, &mut buffer, ctx.protocol_version).await {
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

async fn quic_uni_stream_reader_task(mut recv: quinn::RecvStream, ctx: PacketReaderContext) {
    let (flow_id, mut buffer) = match try_read_server_flow_header(&mut recv).await {
        Ok(result) => {
            let flow_id = if let (Some(id), Some(flags)) = (result.flow_id, result.flags) {
                tracing::debug!(
                    flow_id = ?id,
                    ?flags,
                    expire = ?result.expire,
                    "Unidirectional server stream with flow header"
                );
                Some(id)
            } else {
                tracing::trace!("No flow header on unidirectional server stream");
                None
            };
            (flow_id, result.leftover)
        }
        Err(e) => {
            tracing::warn!("Error parsing server flow header on uni stream: {e}");
            (None, BytesMut::new())
        }
    };

    loop {
        match read_packet_with_buffer(&mut recv, &mut buffer, ctx.protocol_version).await {
            Ok(packet) => {
                tracing::trace!(flow_id = ?flow_id, "Received packet on unidirectional server stream");
                if let Err(e) = handle_incoming_packet_no_writer(
                    packet,
                    &ctx.callback_manager,
                    flow_id,
                    &ctx.keepalive_state,
                    ctx.codec_registry.as_ref(),
                )
                .await
                {
                    tracing::error!(flow_id = ?flow_id, "Error handling packet from uni stream: {e}");
                    break;
                }
            }
            Err(e) => {
                tracing::debug!(flow_id = ?flow_id, "Unidirectional server stream closed: {e}");
                break;
            }
        }
    }
}
