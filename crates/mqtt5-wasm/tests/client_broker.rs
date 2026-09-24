#![cfg(all(target_arch = "wasm32", feature = "broker"))]

use mqtt5_wasm::{
    WasmBroker, WasmBrokerConfig, WasmConnectOptions, WasmMqttClient, WasmPublishOptions,
    WasmSubscribeOptions,
};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

async fn sleep(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _| {
        let set_timeout = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("setTimeout"))
            .unwrap()
            .unchecked_into::<js_sys::Function>();
        set_timeout
            .call2(&JsValue::NULL, &resolve, &JsValue::from(ms))
            .unwrap();
    });
    JsFuture::from(promise).await.unwrap();
}

fn publish_options(qos: u8) -> WasmPublishOptions {
    let mut options = WasmPublishOptions::new();
    options.set_qos(qos);
    options
}

#[wasm_bindgen_test]
async fn wasm_client_against_wasm_broker() {
    let mut config = WasmBrokerConfig::new();
    config.set_allow_anonymous(true);
    let broker = WasmBroker::with_config(config).unwrap();

    let subscriber = WasmMqttClient::new("sub".to_string());
    subscriber
        .connect_message_port_with_options(
            broker.create_client_port().unwrap(),
            &WasmConnectOptions::new(),
        )
        .await
        .unwrap();
    let received = Rc::new(RefCell::new(Vec::<(String, u32)>::new()));
    let sink = Rc::clone(&received);
    let callback = Closure::<dyn FnMut(JsValue, JsValue, JsValue)>::new(
        move |topic: JsValue, payload: JsValue, _: JsValue| {
            let length = js_sys::Uint8Array::new(&payload).length();
            sink.borrow_mut()
                .push((topic.as_string().unwrap_or_default(), length));
        },
    );
    let mut subscribe_options = WasmSubscribeOptions::new();
    subscribe_options.set_qos(2);
    subscriber
        .subscribe_with_options(
            "t/#",
            callback.into_js_value().unchecked_into(),
            &subscribe_options,
        )
        .await
        .unwrap();

    let publisher = WasmMqttClient::new("pub".to_string());
    publisher
        .connect_message_port_with_options(
            broker.create_client_port().unwrap(),
            &WasmConnectOptions::new(),
        )
        .await
        .unwrap();
    publisher.publish("t/0", b"").await.unwrap();
    publisher
        .publish_with_options("t/1", b"a", &publish_options(1))
        .await
        .unwrap();
    publisher
        .publish_with_options("t/2", &vec![7u8; 200_000], &publish_options(2))
        .await
        .unwrap();
    for i in 0..50u8 {
        publisher
            .publish_with_options(&format!("t/m{i}"), b"x", &publish_options(i % 3))
            .await
            .unwrap();
    }
    for _ in 0..200 {
        if received.borrow().len() >= 53 {
            break;
        }
        sleep(10).await;
    }
    let got = received.borrow().clone();
    assert_eq!(got.len(), 53, "{got:?}");
    assert!(got.contains(&("t/2".to_string(), 200_000)));
    assert!(publisher.is_connected() && subscriber.is_connected());
    publisher.disconnect().await.unwrap();
    subscriber.disconnect().await.unwrap();
}
