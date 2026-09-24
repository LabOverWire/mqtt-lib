use bytes::BytesMut;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::Transport;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;
use wasm_bindgen::JsValue;

use crate::transport::{WasmReader, WasmTransportType};

use super::callbacks::{discard_session, SESSION_DISCARDED_BY_CLEAN_START};
use super::handlers::handle_auth;
use super::keepalive::spawn_keepalive_task;
use super::packet::{encode_packet, write_packet};
use super::qos::{resume_session, spawn_qos2_cleanup_task};
use super::reader::{read_connect_response, spawn_packet_reader};
use super::state::{ClientState, SessionState, StoredConnectOptions};

pub enum ConnectFailure {
    Redirect(String),
    Failed(String),
}

impl fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Redirect(url) => write!(f, "Server redirected the client to {url}"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

impl From<ConnectFailure> for JsValue {
    fn from(failure: ConnectFailure) -> Self {
        match failure {
            ConnectFailure::Redirect(url) => {
                let obj = js_sys::Object::new();
                js_sys::Reflect::set(
                    &obj,
                    &JsValue::from_str("type"),
                    &JsValue::from_str("redirect"),
                )
                .ok();
                js_sys::Reflect::set(&obj, &JsValue::from_str("url"), &JsValue::from_str(&url))
                    .ok();
                obj.into()
            }
            ConnectFailure::Failed(message) => JsValue::from_str(&message),
        }
    }
}

pub async fn establish(
    state: &Rc<RefCell<ClientState>>,
    mut transport: WasmTransportType,
    connect: ConnectPacket,
    options: &StoredConnectOptions,
) -> Result<ConnAckPacket, ConnectFailure> {
    transport
        .connect()
        .await
        .map_err(|e| ConnectFailure::Failed(format!("Transport connection failed: {e}")))?;

    if connect.clean_start {
        state.borrow_mut().quarantined.clear();
        discard_session(state, SESSION_DISCARDED_BY_CLEAN_START);
    }
    state
        .borrow_mut()
        .apply_connect_options(options, connect.clean_start);

    let mut buf = BytesMut::new();
    encode_packet(&Packet::Connect(Box::new(connect.clone())), &mut buf)
        .map_err(|e| ConnectFailure::Failed(format!("Packet encoding failed: {e}")))?;
    transport
        .write(&buf)
        .await
        .map_err(|e| ConnectFailure::Failed(format!("Write failed: {e}")))?;

    let (mut reader, writer) = transport
        .into_split()
        .map_err(|e| ConnectFailure::Failed(format!("Transport split failed: {e}")))?;
    state.borrow_mut().writer = Some(Rc::new(RefCell::new(writer)));

    match await_connack(state, &mut reader).await {
        Ok(connack) => accept_connack(
            state,
            reader,
            connack,
            connect.clean_start,
            options.resume_existing_session,
        ),
        Err(failure) => {
            drop_writer(state);
            Err(failure)
        }
    }
}

fn drop_writer(state: &Rc<RefCell<ClientState>>) {
    let writer = state.borrow_mut().writer.take();
    if let Some(writer) = writer {
        if let Err(e) = writer.borrow_mut().close() {
            tracing::warn!(error = %e, "closing the network connection failed");
        }
    }
}

async fn await_connack(
    state: &Rc<RefCell<ClientState>>,
    reader: &mut WasmReader,
) -> Result<ConnAckPacket, ConnectFailure> {
    loop {
        let packet = read_connect_response(state, reader)
            .await
            .map_err(|e| ConnectFailure::Failed(format!("Packet read failed: {e}")))?;
        match packet {
            Packet::ConnAck(connack) if connack.reason_code == ReasonCode::Success => {
                return Ok(connack);
            }
            Packet::ConnAck(connack) => {
                let reason_code = u8::from(connack.reason_code);
                return Err(
                    match (reason_code, connack.properties.get_server_reference()) {
                        (0x9C | 0x9D, Some(server_reference)) => {
                            ConnectFailure::Redirect(server_reference.to_string())
                        }
                        _ => ConnectFailure::Failed(format!(
                            "Connection rejected: {}",
                            connack_error_description(reason_code)
                        )),
                    },
                );
            }
            Packet::Auth(auth) => handle_auth(state, &auth).map_err(|violation| {
                ConnectFailure::Failed(format!(
                    "Authentication failed ({:?}): {}",
                    violation.reason_code, violation.message
                ))
            })?,
            other => {
                return Err(ConnectFailure::Failed(format!(
                    "Expected CONNACK or AUTH, received {}",
                    other.packet_type_name()
                )));
            }
        }
    }
}

fn accept_connack(
    state: &Rc<RefCell<ClientState>>,
    reader: WasmReader,
    connack: ConnAckPacket,
    clean_start: bool,
    resume_existing_session: bool,
) -> Result<ConnAckPacket, ConnectFailure> {
    let (had_session, protocol_version) = {
        let state_ref = state.borrow();
        (
            !clean_start && (resume_existing_session || state_ref.session == SessionState::Held),
            state_ref.protocol_version,
        )
    };

    if connack.session_present && !had_session {
        if protocol_version == 5 {
            let disconnect = DisconnectPacket::new(ReasonCode::ProtocolError);
            if let Err(e) = write_packet(state, &Packet::Disconnect(disconnect)) {
                tracing::warn!(error = %e, "DISCONNECT not sent");
            }
        }
        drop_writer(state);
        return Err(ConnectFailure::Failed(
            "Server reported Session Present but the client holds no session state".to_string(),
        ));
    }

    {
        let mut state_mut = state.borrow_mut();
        state_mut.apply_connack(&connack);
        state_mut.connected = true;
        state_mut.session = SessionState::Held;
        state_mut.connection_generation = state_mut.connection_generation.wrapping_add(1);
    }

    resume_session(state, connack.session_present);

    spawn_packet_reader(Rc::clone(state), reader);
    spawn_keepalive_task(Rc::clone(state));
    spawn_qos2_cleanup_task(Rc::clone(state));

    let callback = state.borrow().on_connect.clone();
    if let Some(callback) = callback {
        let reason_code_js = JsValue::from_f64(f64::from(u8::from(connack.reason_code)));
        let session_present_js = JsValue::from_bool(connack.session_present);
        if let Err(e) = callback.call2(&JsValue::NULL, &reason_code_js, &session_present_js) {
            tracing::warn!(error = ?e, "onConnect callback failed");
        }
    }

    Ok(connack)
}

fn connack_error_description(reason_code: u8) -> &'static str {
    match reason_code {
        0x80 => "Unspecified error",
        0x81 => "Malformed packet",
        0x82 => "Protocol error",
        0x83 => "Implementation specific error",
        0x84 => "Unsupported protocol version",
        0x85 => "Client identifier not valid",
        0x86 => "Bad username or password",
        0x87 => "Not authorized",
        0x88 => "Server unavailable",
        0x89 => "Server busy",
        0x8A => "Banned",
        0x8C => "Bad authentication method",
        0x90 => "Topic name invalid",
        0x97 => "Quota exceeded",
        0x9C => "Use another server",
        0x9D => "Server moved",
        0x9F => "Connection rate exceeded",
        _ => "Unknown error",
    }
}
