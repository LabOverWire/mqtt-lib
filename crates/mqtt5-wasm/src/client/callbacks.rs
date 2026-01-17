use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

use super::state::ClientState;

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
