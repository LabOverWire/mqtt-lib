//! Packet reading loop and QUIC stream handling

use crate::callback::CallbackManager;
use crate::client::auth_handler::{AuthHandler, AuthResponse};
use crate::client::connection::DisconnectReason;
use crate::codec::CodecRegistry;
use crate::error::{MqttError, Result};
use crate::packet::auth::AuthPacket;
use crate::packet::disconnect::DisconnectPacket;
use crate::packet::suback::SubAckPacket;
use crate::packet::unsuback::UnsubAckPacket;
use crate::packet::Packet;
use crate::protocol::v5::properties::{Properties, PropertyId};
use crate::protocol::v5::reason_codes::ReasonCode;
use crate::session::state::OutboundStage;
use crate::session::{SessionState, TopicAliasManager};
use crate::transport::PacketWriter;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::oneshot;

use super::handlers::handle_incoming_packet_with_writer;
use super::keepalive::{ConnectionLifecycle, KeepaliveState};
use super::unified::{UnifiedReader, UnifiedWriter};

#[cfg(feature = "transport-quic")]
use super::handlers::handle_incoming_packet_no_writer;
#[cfg(feature = "transport-quic")]
use crate::transport::flow::{
    FlowFlags, FlowHeader, FlowId, FLOW_TYPE_CLIENT_DATA, FLOW_TYPE_CONTROL, FLOW_TYPE_SERVER_DATA,
};
#[cfg(feature = "transport-quic")]
use crate::transport::packet_io::read_packet_from_stream;
#[cfg(feature = "transport-quic")]
use bytes::{Buf, Bytes, BytesMut};
#[cfg(feature = "transport-quic")]
use quinn::Connection;
#[cfg(feature = "transport-quic")]
use std::time::Duration as StdDuration;

const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone)]
pub(super) struct PacketReaderContext {
    pub(super) session: Arc<tokio::sync::RwLock<SessionState>>,
    pub(super) callback_manager: Arc<CallbackManager>,
    pub(super) suback_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<SubAckPacket>>>>,
    pub(super) unsuback_channels: Arc<Mutex<HashMap<u16, oneshot::Sender<UnsubAckPacket>>>>,
    pub(super) publish_outcomes: super::tracking::SharedOutcomes,
    pub(super) writer: Arc<tokio::sync::Mutex<UnifiedWriter>>,
    pub(super) lifecycle: ConnectionLifecycle,
    #[cfg(feature = "transport-quic")]
    pub(super) protocol_version: u8,
    #[cfg(feature = "transport-quic")]
    pub(super) maximum_packet_size: usize,
    pub(super) auth_handler: Option<Arc<dyn AuthHandler>>,
    pub(super) auth_method: Option<String>,
    pub(super) keepalive_state: Arc<Mutex<KeepaliveState>>,
    pub(super) codec_registry: Option<Arc<CodecRegistry>>,
    pub(super) deferred_ack: bool,
    pub(super) ack_callbacks: Arc<super::ack::AckCallbackManager>,
    pub(super) ack_dispatcher: Arc<super::ack::AckDispatcher>,
    pub(super) topic_aliases: Arc<Mutex<TopicAliasManager>>,
    pub(super) request_problem_information: bool,
}

impl PacketReaderContext {
    fn ack_delivery(&self) -> Option<super::handlers::AckDelivery<'_>> {
        if self.deferred_ack {
            Some(super::handlers::AckDelivery {
                callbacks: &self.ack_callbacks,
                dispatcher: &self.ack_dispatcher,
            })
        } else {
            None
        }
    }

    fn incoming_handlers<'a>(
        &'a self,
        ack_delivery: Option<&'a super::handlers::AckDelivery<'a>>,
    ) -> super::handlers::IncomingHandlers<'a> {
        super::handlers::IncomingHandlers {
            session: &self.session,
            topic_aliases: &self.topic_aliases,
            callback_manager: &self.callback_manager,
            keepalive_state: &self.keepalive_state,
            codec_registry: self.codec_registry.as_ref(),
            ack_delivery,
        }
    }
}

impl PacketReaderContext {
    fn clear_pending_if_current(&self) {
        if !self.lifecycle.owns_current_connection() {
            return;
        }
        self.suback_channels.lock().drain();
        self.unsuback_channels.lock().drain();
    }
}

fn disconnect_reason_for(error: &MqttError) -> DisconnectReason {
    match error {
        MqttError::ServerDisconnect(reason_code) => {
            DisconnectReason::ServerDisconnect(*reason_code)
        }
        MqttError::AuthenticationFailed | MqttError::NotAuthorized => DisconnectReason::AuthFailure,
        MqttError::KeepAliveTimeout => DisconnectReason::KeepAliveTimeout,
        MqttError::Io(_)
        | MqttError::ConnectionError(_)
        | MqttError::ConnectionClosedByPeer
        | MqttError::ClientClosed => DisconnectReason::NetworkError(error.to_string()),
        _ => DisconnectReason::ProtocolError(error.to_string()),
    }
}

fn disconnect_code_for(error: &MqttError) -> Option<ReasonCode> {
    match error {
        MqttError::Io(_)
        | MqttError::ConnectionError(_)
        | MqttError::ConnectionClosedByPeer
        | MqttError::ClientClosed
        | MqttError::NotConnected
        | MqttError::ServerDisconnect(_)
        | MqttError::KeepAliveTimeout => None,
        MqttError::MalformedPacket(_)
        | MqttError::InvalidQoS(_)
        | MqttError::InvalidPacketType(_)
        | MqttError::InvalidReasonCode(_)
        | MqttError::InvalidPropertyId(_)
        | MqttError::InvalidTopicName(_)
        | MqttError::StringTooLong(_) => Some(ReasonCode::MalformedPacket),
        MqttError::ProtocolError(_) | MqttError::DuplicatePropertyId(_) => {
            Some(ReasonCode::ProtocolError)
        }
        MqttError::PacketTooLarge { .. } => Some(ReasonCode::PacketTooLarge),
        MqttError::ReceiveMaximumExceeded => Some(ReasonCode::ReceiveMaximumExceeded),
        MqttError::TopicAliasInvalid(_) => Some(ReasonCode::TopicAliasInvalid),
        MqttError::AuthenticationFailed | MqttError::NotAuthorized => {
            Some(ReasonCode::NotAuthorized)
        }
        _ => Some(ReasonCode::UnspecifiedError),
    }
}

fn problem_information_properties(packet: &Packet) -> Option<&Properties> {
    match packet {
        Packet::PubAck(p) => Some(&p.properties),
        Packet::PubRec(p) => Some(&p.properties),
        Packet::PubRel(p) => Some(&p.properties),
        Packet::PubComp(p) => Some(&p.properties),
        Packet::SubAck(p) => Some(&p.properties),
        Packet::UnsubAck(p) => Some(&p.properties),
        Packet::Auth(p) => Some(&p.properties),
        _ => None,
    }
}

fn check_problem_information(packet: &Packet, requested: bool) -> Result<()> {
    let Some(properties) = problem_information_properties(packet).filter(|_| !requested) else {
        return Ok(());
    };
    if properties.contains(PropertyId::ReasonString)
        || properties.contains(PropertyId::UserProperty)
    {
        return Err(MqttError::ProtocolError(format!(
            "{} carries a Reason String or User Property although Request Problem Information is 0 [MQTT-3.1.2-29]",
            packet.packet_type_name()
        )));
    }
    Ok(())
}

pub(super) async fn close_connection(
    writer: &Arc<tokio::sync::Mutex<UnifiedWriter>>,
    lifecycle: &ConnectionLifecycle,
    error: &MqttError,
) {
    lifecycle.begin_close();
    let disconnect = disconnect_code_for(error)
        .map(|reason_code| Packet::Disconnect(DisconnectPacket::new(reason_code)));
    let closing = async { writer.lock().await.close(disconnect).await };
    match tokio::time::timeout(CLOSE_TIMEOUT, closing).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::debug!("Closing network connection: {e}"),
        Err(_) => tracing::warn!("Timed out closing network connection"),
    }
}

async fn check_acknowledgement_matches(packet: &Packet, ctx: &PacketReaderContext) -> Result<()> {
    let (packet_id, name, accepted): (u16, &str, &[OutboundStage]) = match packet {
        Packet::PubAck(ack) => (ack.packet_id, "PUBACK", &[OutboundStage::AwaitingPubAck]),
        Packet::PubRec(ack) if ack.reason_code.is_error() => {
            (ack.packet_id, "PUBREC", &[OutboundStage::AwaitingPubRec])
        }
        Packet::PubRec(ack) => (
            ack.packet_id,
            "PUBREC",
            &[
                OutboundStage::AwaitingPubRec,
                OutboundStage::AwaitingPubComp,
            ],
        ),
        Packet::PubComp(ack) => (ack.packet_id, "PUBCOMP", &[OutboundStage::AwaitingPubComp]),
        _ => return Ok(()),
    };
    match ctx.session.read().await.outbound_stage(packet_id).await {
        Some(stage) if !accepted.contains(&stage) => Err(MqttError::ProtocolError(format!(
            "{name} for packet identifier {packet_id} which is waiting for {stage:?}"
        ))),
        _ => Ok(()),
    }
}

enum Routed {
    Settled,
    Unhandled,
}

async fn settle_acknowledgement(packet: &Packet, ctx: &PacketReaderContext) -> Result<Routed> {
    check_acknowledgement_matches(packet, ctx).await?;
    match packet {
        Packet::PubAck(puback) => {
            ctx.publish_outcomes
                .lock()
                .settle_puback(puback.packet_id, puback.reason_code);
            super::DirectClientInner::release_outbound_quota(&ctx.session, Some(puback.packet_id))
                .await;
            Ok(Routed::Settled)
        }
        Packet::PubRec(pubrec) if pubrec.reason_code.is_error() => {
            tracing::debug!(
                packet_id = pubrec.packet_id,
                reason_code = ?pubrec.reason_code,
                "QoS 2 PUBREC rejected"
            );
            ctx.session
                .write()
                .await
                .remove_unacked_publish(pubrec.packet_id)
                .await;
            ctx.publish_outcomes.lock().refused(
                pubrec.packet_id,
                crate::QoS::ExactlyOnce,
                pubrec.reason_code,
            );
            super::DirectClientInner::release_outbound_quota(&ctx.session, Some(pubrec.packet_id))
                .await;
            Ok(Routed::Settled)
        }
        Packet::PubComp(pubcomp) => {
            ctx.publish_outcomes
                .lock()
                .acknowledged(pubcomp.packet_id, crate::QoS::ExactlyOnce);
            super::DirectClientInner::release_outbound_quota(&ctx.session, Some(pubcomp.packet_id))
                .await;
            Ok(Routed::Settled)
        }
        _ => Ok(Routed::Unhandled),
    }
}

async fn process_packet(packet: Packet, ctx: &PacketReaderContext) -> Result<()> {
    tracing::trace!("Received packet: {:?}", packet);
    check_problem_information(&packet, ctx.request_problem_information)?;
    if let Routed::Settled = settle_acknowledgement(&packet, ctx).await? {
        return Ok(());
    }
    match &packet {
        Packet::SubAck(suback) => {
            if let Some(tx) = ctx.suback_channels.lock().remove(&suback.packet_id) {
                let _ = tx.send(suback.clone());
                return Ok(());
            }
        }
        Packet::UnsubAck(unsuback) => {
            if let Some(tx) = ctx.unsuback_channels.lock().remove(&unsuback.packet_id) {
                let _ = tx.send(unsuback.clone());
                return Ok(());
            }
        }
        Packet::Auth(auth) => return handle_auth_packet(auth.clone(), ctx).await,
        _ => {}
    }

    let ack_delivery = ctx.ack_delivery();
    let handlers = ctx.incoming_handlers(ack_delivery.as_ref());
    handle_incoming_packet_with_writer(packet, &ctx.writer, None, &handlers).await
}

pub(super) async fn packet_reader_task_with_responses(
    mut reader: UnifiedReader,
    ctx: PacketReaderContext,
) {
    tracing::debug!("Packet reader task started and ready to process incoming packets");
    let failure = loop {
        let outcome = match reader.read_packet().await {
            Ok(packet) => process_packet(packet, &ctx).await,
            Err(e) => Err(e),
        };
        if let Err(e) = outcome {
            break e;
        }
    };

    tracing::error!("Packet reader stopping: {failure}");
    close_connection(&ctx.writer, &ctx.lifecycle, &failure).await;
    drop(reader);
    ctx.lifecycle.end(disconnect_reason_for(&failure)).await;
    ctx.clear_pending_if_current();
}

async fn handle_auth_packet(auth: AuthPacket, ctx: &PacketReaderContext) -> Result<()> {
    tracing::debug!(
        "CLIENT: Received AUTH during session with reason: {:?}",
        auth.reason_code
    );

    match auth.reason_code {
        ReasonCode::ContinueAuthentication => {
            let method = ctx.auth_method.clone().ok_or_else(|| {
                MqttError::ProtocolError(
                    "AUTH received but CONNECT carried no Authentication Method".to_string(),
                )
            })?;
            let handler = ctx
                .auth_handler
                .as_ref()
                .ok_or(MqttError::AuthenticationFailed)?;

            let auth_method = auth.authentication_method().unwrap_or("");
            let auth_data = auth.authentication_data();

            let response = handler.handle_challenge(auth_method, auth_data).await?;

            match response {
                AuthResponse::Continue(data) => {
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

#[cfg(feature = "transport-quic")]
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
                        ctx.lifecycle
                            .end(DisconnectReason::NetworkError(reason.to_string()))
                            .await;
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
                        ctx.lifecycle
                            .end(DisconnectReason::NetworkError(reason.to_string()))
                            .await;
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(feature = "transport-quic")]
fn is_flow_header_byte(b: u8) -> bool {
    matches!(
        b,
        FLOW_TYPE_CONTROL | FLOW_TYPE_CLIENT_DATA | FLOW_TYPE_SERVER_DATA
    )
}

#[cfg(feature = "transport-quic")]
struct ServerFlowResult {
    flow_id: Option<FlowId>,
    flags: Option<FlowFlags>,
    expire: Option<StdDuration>,
    leftover: BytesMut,
}

#[cfg(feature = "transport-quic")]
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

#[cfg(feature = "transport-quic")]
async fn read_data_stream_packet(
    recv: &mut quinn::RecvStream,
    buffer: &mut BytesMut,
    ctx: &PacketReaderContext,
) -> Option<Result<Packet>> {
    let read =
        read_packet_from_stream(recv, ctx.protocol_version, buffer, ctx.maximum_packet_size).await;
    match read {
        Ok(_) if ctx.lifecycle.is_closing() => None,
        read => Some(read),
    }
}

#[cfg(feature = "transport-quic")]
async fn end_data_stream(ctx: &PacketReaderContext, flow_id: Option<FlowId>, error: &MqttError) {
    let fails_connection =
        matches!(error, MqttError::ServerDisconnect(_)) || disconnect_code_for(error).is_some();
    if !fails_connection {
        tracing::debug!(flow_id = ?flow_id, "Server QUIC data stream closed: {error}");
        return;
    }
    tracing::error!(flow_id = ?flow_id, "Server QUIC data stream failed the connection: {error}");
    close_connection(&ctx.writer, &ctx.lifecycle, error).await;
    ctx.lifecycle.end(disconnect_reason_for(error)).await;
    ctx.clear_pending_if_current();
}

#[cfg(feature = "transport-quic")]
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

    while let Some(read) = read_data_stream_packet(&mut recv, &mut buffer, &ctx).await {
        let outcome = match read {
            Ok(packet) => {
                tracing::trace!(flow_id = ?flow_id, "Received packet on server-initiated QUIC stream: {:?}", packet);
                match settle_acknowledgement(&packet, &ctx).await {
                    Ok(Routed::Settled) => Ok(()),
                    Ok(Routed::Unhandled) => {
                        let ack_delivery = ctx.ack_delivery();
                        let handlers = ctx.incoming_handlers(ack_delivery.as_ref());
                        handle_incoming_packet_with_writer(
                            packet,
                            &stream_writer,
                            flow_id,
                            &handlers,
                        )
                        .await
                    }
                    Err(e) => Err(e),
                }
            }
            Err(e) => Err(e),
        };
        if let Err(e) = outcome {
            end_data_stream(&ctx, flow_id, &e).await;
            break;
        }
    }
}

#[cfg(feature = "transport-quic")]
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

    while let Some(read) = read_data_stream_packet(&mut recv, &mut buffer, &ctx).await {
        let outcome = match read {
            Ok(packet) => {
                tracing::trace!(flow_id = ?flow_id, "Received packet on unidirectional server stream");
                handle_incoming_packet_no_writer(packet, flow_id, &ctx.incoming_handlers(None))
                    .await
            }
            Err(e) => Err(e),
        };
        if let Err(e) = outcome {
            end_data_stream(&ctx, flow_id, &e).await;
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::keepalive::owns_current_connection;
    use super::disconnect_reason_for;
    use crate::client::connection::DisconnectReason;
    use crate::error::MqttError;
    use crate::protocol::v5::reason_codes::ReasonCode;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn stale_epoch_does_not_own_current_connection() {
        assert!(!owns_current_connection(1, &AtomicU64::new(2)));
    }

    #[test]
    fn disconnect_reason_carries_server_reason_code() {
        assert_eq!(
            disconnect_reason_for(&MqttError::ServerDisconnect(ReasonCode::SessionTakenOver)),
            DisconnectReason::ServerDisconnect(ReasonCode::SessionTakenOver)
        );
        assert_eq!(
            disconnect_reason_for(&MqttError::AuthenticationFailed),
            DisconnectReason::AuthFailure
        );
        assert_eq!(
            disconnect_reason_for(&MqttError::KeepAliveTimeout),
            DisconnectReason::KeepAliveTimeout
        );
        assert!(matches!(
            disconnect_reason_for(&MqttError::ConnectionClosedByPeer),
            DisconnectReason::NetworkError(_)
        ));
        assert!(matches!(
            disconnect_reason_for(&MqttError::MalformedPacket("bad".to_string())),
            DisconnectReason::ProtocolError(_)
        ));
    }
}
