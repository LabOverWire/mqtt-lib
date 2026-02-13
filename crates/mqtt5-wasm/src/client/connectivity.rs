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

    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_once(true);

    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let callback = Closure::once_into_js(move || {
            let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
        });
        let _ = window.add_event_listener_with_callback_and_add_event_listener_options(
            "online",
            callback.as_ref().unchecked_ref(),
            &opts,
        );
    });

    let _ = JsFuture::from(promise).await;
}

pub fn register_connectivity_listeners(
    state: &Rc<RefCell<ClientState>>,
) -> (js_sys::Function, js_sys::Function) {
    let Some(window) = web_sys::window() else {
        let noop = js_sys::Function::new_no_args("");
        return (noop.clone(), noop);
    };

    let state_online = Rc::clone(state);
    let online_closure = Closure::<dyn FnMut()>::new(move || {
        trigger_connectivity_change_callback(&state_online, true);
    });
    let online_fn: js_sys::Function = online_closure
        .as_ref()
        .unchecked_ref::<js_sys::Function>()
        .clone();
    let _ = window.add_event_listener_with_callback("online", &online_fn);
    online_closure.forget();

    let state_offline = Rc::clone(state);
    let offline_closure = Closure::<dyn FnMut()>::new(move || {
        trigger_connectivity_change_callback(&state_offline, false);
    });
    let offline_fn: js_sys::Function = offline_closure
        .as_ref()
        .unchecked_ref::<js_sys::Function>()
        .clone();
    let _ = window.add_event_listener_with_callback("offline", &offline_fn);
    offline_closure.forget();

    (online_fn, offline_fn)
}

pub fn remove_connectivity_listeners(online_fn: &js_sys::Function, offline_fn: &js_sys::Function) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let _ = window.remove_event_listener_with_callback("online", online_fn);
    let _ = window.remove_event_listener_with_callback("offline", offline_fn);
}
