use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

use super::reconnect::spawn_reconnection_task;
use super::state::ClientState;

pub fn handle_connection_lost(state: &Rc<RefCell<ClientState>>, reason: &str) {
    let (should_reconnect, pubacks, pubcomps, subacks) = {
        let mut state_ref = state.borrow_mut();
        if !state_ref.connected {
            return;
        }
        state_ref.connected = false;

        let pubacks: Vec<_> = state_ref.pending_pubacks.drain().collect();
        let pubcomps: Vec<_> = state_ref.pending_pubcomps.drain().collect();
        let subacks: Vec<_> = state_ref.pending_subacks.drain().collect();

        let should = state_ref.reconnect_config.enabled
            && !state_ref.user_initiated_disconnect
            && !state_ref.reconnecting
            && state_ref.last_url.is_some();
        (should, pubacks, pubcomps, subacks)
    };

    let error_val = JsValue::from_f64(f64::from(0x80_u8));
    for (_, callback) in pubacks {
        let _ = callback.call1(&JsValue::NULL, &error_val);
    }
    for (_, (callback, _)) in pubcomps {
        let _ = callback.call1(&JsValue::NULL, &error_val);
    }
    for (_, resolve) in subacks {
        let arr = js_sys::Array::new();
        arr.push(&JsValue::from_f64(f64::from(0x80_u8)));
        let _ = resolve.call1(&JsValue::NULL, &arr.into());
    }

    web_sys::console::error_1(&reason.into());
    trigger_error_callback(state, reason);
    trigger_disconnect_callback(state);

    if should_reconnect {
        spawn_reconnection_task(Rc::clone(state));
    }
}

pub fn trigger_disconnect_callback(state: &Rc<RefCell<ClientState>>) {
    let callback = state.borrow().on_disconnect.clone();
    if let Some(callback) = callback {
        if let Err(e) = callback.call0(&JsValue::NULL) {
            web_sys::console::error_1(&format!("onDisconnect callback error: {e:?}").into());
        }
    }
}

pub fn trigger_error_callback(state: &Rc<RefCell<ClientState>>, error_msg: &str) {
    let callback = state.borrow().on_error.clone();
    if let Some(callback) = callback {
        let error_js = JsValue::from_str(error_msg);
        if let Err(e) = callback.call1(&JsValue::NULL, &error_js) {
            web_sys::console::error_1(&format!("onError callback error: {e:?}").into());
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
            web_sys::console::error_1(&format!("onReconnecting callback error: {e:?}").into());
        }
    }
}

pub fn trigger_reconnect_failed_callback(state: &Rc<RefCell<ClientState>>, error_msg: &str) {
    let callback = state.borrow().on_reconnect_failed.clone();
    if let Some(callback) = callback {
        let error_js = JsValue::from_str(error_msg);
        if let Err(e) = callback.call1(&JsValue::NULL, &error_js) {
            web_sys::console::error_1(&format!("onReconnectFailed callback error: {e:?}").into());
        }
    }
}
