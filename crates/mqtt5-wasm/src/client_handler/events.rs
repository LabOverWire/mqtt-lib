use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;

pub fn set_prop(obj: &js_sys::Object, key: &str, value: &JsValue) {
    let _ = js_sys::Reflect::set(obj, &key.into(), value);
}

pub fn fire_event<F>(callback: Option<js_sys::Function>, name: &'static str, build_obj: F)
where
    F: FnOnce(&js_sys::Object) + 'static,
{
    if let Some(cb) = callback {
        spawn_local(async move {
            let obj = js_sys::Object::new();
            build_obj(&obj);
            if let Err(e) = cb.call1(&JsValue::NULL, &obj) {
                tracing::debug!("{} callback failed: {:?}", name, e);
            }
        });
    }
}
