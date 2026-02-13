use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use super::callbacks::trigger_connectivity_change_callback;
use super::state::ClientState;

pub fn is_browser_online() -> bool {
    web_sys::window()
        .and_then(|w| w.navigator().on_line().into())
        .unwrap_or(true)
}

pub async fn wait_for_online() {
    if is_browser_online() {
        return;
    }

    let Some(window) = web_sys::window() else {
        return;
    };

    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let callback = Closure::once_into_js(move || {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });
        let _ =
            window.add_event_listener_with_callback("online", callback.as_ref().unchecked_ref());
    });

    let _ = JsFuture::from(promise).await;
}

pub fn register_connectivity_listeners(state: &Rc<RefCell<ClientState>>) {
    let Some(window) = web_sys::window() else {
        return;
    };

    let state_online = Rc::clone(state);
    let online_closure = Closure::<dyn FnMut()>::new(move || {
        trigger_connectivity_change_callback(&state_online, true);
    });
    let _ =
        window.add_event_listener_with_callback("online", online_closure.as_ref().unchecked_ref());
    online_closure.forget();

    let state_offline = Rc::clone(state);
    let offline_closure = Closure::<dyn FnMut()>::new(move || {
        trigger_connectivity_change_callback(&state_offline, false);
    });
    let _ = window
        .add_event_listener_with_callback("offline", offline_closure.as_ref().unchecked_ref());
    offline_closure.forget();
}
