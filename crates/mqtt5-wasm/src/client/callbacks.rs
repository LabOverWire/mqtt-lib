use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::Packet;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

use super::handlers::Violation;
use super::packet::write_packet;
use super::qos::wake_quota_waiters;
use super::reconnect::spawn_reconnection_task;
use super::state::ClientState;

const SESSION_DISCARDED_REASON: u8 = 0x80;
const DISCONNECTED_WITH_SESSION: &str =
    "disconnected; message remains in session and will be resent on resume by this client instance";

pub const SESSION_DISCARDED_BY_CLEAN_START: &str =
    "indeterminate: session discarded by clean start; may have been delivered";
const SESSION_ENDED_WITH_CONNECTION: &str =
    "indeterminate: session ended with the connection; may have been delivered";

fn settle_callbacks(callbacks: Vec<js_sys::Function>, message: &str) {
    let message = JsValue::from_str(message);
    for callback in callbacks {
        if let Err(e) = callback.call1(&JsValue::NULL, &message) {
            tracing::warn!(error = ?e, "acknowledgement callback failed");
        }
    }
}

pub fn settle_acks_on_disconnect(state: &Rc<RefCell<ClientState>>) {
    let callbacks = state.borrow_mut().take_ack_callbacks();
    settle_callbacks(callbacks, DISCONNECTED_WITH_SESSION);
}

pub fn discard_session(state: &Rc<RefCell<ClientState>>, message: &str) {
    let (callbacks, abandoned) = {
        let mut state_mut = state.borrow_mut();
        let abandoned = state_mut.outbound.len();
        (state_mut.discard_session(), abandoned)
    };
    if abandoned > 0 {
        tracing::warn!(
            abandoned,
            reason = message,
            "outbound session state discarded"
        );
    }
    settle_callbacks(callbacks, message);
}

pub fn close_network_connection(state: &Rc<RefCell<ClientState>>) {
    let writer = {
        let mut state_mut = state.borrow_mut();
        state_mut.connected = false;
        state_mut.connection_generation = state_mut.connection_generation.wrapping_add(1);
        state_mut.writer.take()
    };
    if let Some(writer) = writer {
        if let Err(e) = writer.borrow_mut().close() {
            tracing::warn!(error = %e, "closing the network connection failed");
        }
    }
}

pub fn end_connection_state(state: &Rc<RefCell<ClientState>>) {
    let (subacks, session_ends) = {
        let mut state_mut = state.borrow_mut();
        state_mut.pending_unsubacks.clear();
        let subacks: Vec<js_sys::Function> = state_mut
            .pending_subacks
            .drain()
            .filter_map(|(_, callback)| callback)
            .collect();
        (subacks, !state_mut.session_outlives_connection())
    };
    for resolve in subacks {
        let codes = js_sys::Array::new();
        codes.push(&JsValue::from_f64(f64::from(SESSION_DISCARDED_REASON)));
        if let Err(e) = resolve.call1(&JsValue::NULL, &codes.into()) {
            tracing::warn!(error = ?e, "SUBACK callback failed");
        }
    }
    if session_ends {
        discard_session(state, SESSION_ENDED_WITH_CONNECTION);
    }
    wake_quota_waiters(state);
}

pub fn handle_connection_lost(state: &Rc<RefCell<ClientState>>, reason: &str) {
    let should_reconnect = {
        let state_ref = state.borrow();
        if !state_ref.connected {
            return;
        }
        state_ref.reconnect_config.enabled
            && !state_ref.user_initiated_disconnect
            && !state_ref.reconnecting
            && state_ref.last_url.is_some()
    };

    close_network_connection(state);
    end_connection_state(state);

    tracing::warn!(reason, "connection lost");
    trigger_error_callback(state, reason);
    trigger_disconnect_callback(state);

    if should_reconnect {
        spawn_reconnection_task(Rc::clone(state));
    }
}

pub fn fail_connection(state: &Rc<RefCell<ClientState>>, violation: &Violation) {
    if !state.borrow().connected {
        return;
    }
    if state.borrow().protocol_version == 5 {
        let disconnect = DisconnectPacket::new(violation.reason_code);
        if let Err(e) = write_packet(state, &Packet::Disconnect(disconnect)) {
            tracing::warn!(error = %e, "DISCONNECT not sent");
        }
    }
    let message = format!(
        "Protocol violation ({:?}): {}",
        violation.reason_code, violation.message
    );
    handle_connection_lost(state, &message);
}

pub fn trigger_disconnect_callback(state: &Rc<RefCell<ClientState>>) {
    let callback = state.borrow().on_disconnect.clone();
    if let Some(callback) = callback {
        if let Err(e) = callback.call0(&JsValue::NULL) {
            tracing::warn!(error = ?e, "onDisconnect callback failed");
        }
    }
}

pub fn trigger_error_callback(state: &Rc<RefCell<ClientState>>, error_msg: &str) {
    let callback = state.borrow().on_error.clone();
    if let Some(callback) = callback {
        let error_js = JsValue::from_str(error_msg);
        if let Err(e) = callback.call1(&JsValue::NULL, &error_js) {
            tracing::warn!(error = ?e, "onError callback failed");
        }
    }
}

pub fn trigger_reconnecting_callback(
    state: &Rc<RefCell<ClientState>>,
    attempt: u32,
    delay_millis: u32,
) {
    let callback = state.borrow().on_reconnecting.clone();
    if let Some(callback) = callback {
        let attempt_js = JsValue::from_f64(f64::from(attempt));
        let delay_js = JsValue::from_f64(f64::from(delay_millis));
        if let Err(e) = callback.call2(&JsValue::NULL, &attempt_js, &delay_js) {
            tracing::warn!(error = ?e, "onReconnecting callback failed");
        }
    }
}

pub fn trigger_connectivity_change_callback(state: &Rc<RefCell<ClientState>>, online: bool) {
    let callback = state.borrow().on_connectivity_change.clone();
    if let Some(callback) = callback {
        let online_js = JsValue::from_bool(online);
        if let Err(e) = callback.call1(&JsValue::NULL, &online_js) {
            tracing::warn!(error = ?e, "onConnectivityChange callback failed");
        }
    }
}

pub fn trigger_reconnect_failed_callback(state: &Rc<RefCell<ClientState>>, error_msg: &str) {
    let callback = state.borrow().on_reconnect_failed.clone();
    if let Some(callback) = callback {
        let error_js = JsValue::from_str(error_msg);
        if let Err(e) = callback.call1(&JsValue::NULL, &error_js) {
            tracing::warn!(error = ?e, "onReconnectFailed callback failed");
        }
    }
}
