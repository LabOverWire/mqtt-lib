#![cfg(all(target_arch = "wasm32", feature = "broker"))]

use bytes::BytesMut;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::types::{ConnectOptions, WillMessage};
use mqtt5_wasm::{
    WasmBroker, WasmBrokerConfig, WasmConnectOptions, WasmMqttClient, WasmSubscribeOptions,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;
use web_sys::{MessageEvent, MessagePort};

const SESSION_EXPIRY: u32 = 60;

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

fn now_ms() -> f64 {
    js_sys::Date::now()
}

async fn sleep_until(deadline_ms: f64) {
    while now_ms() < deadline_ms {
        sleep(10).await;
    }
}

fn broker() -> WasmBroker {
    let mut config = WasmBrokerConfig::new();
    config.set_allow_anonymous(true);
    WasmBroker::with_config(config).unwrap()
}

fn will_topic(client_id: &str) -> String {
    format!("will/{client_id}")
}

fn delayed_will(client_id: &str, delay: u32) -> WillMessage {
    let mut will = WillMessage::new(will_topic(client_id), "offline");
    will.properties.will_delay_interval = Some(delay);
    will
}

struct RawClient {
    port: MessagePort,
    inbox: Rc<RefCell<Vec<u8>>>,
    on_message: Closure<dyn FnMut(MessageEvent)>,
}

impl RawClient {
    async fn connect(
        broker: &WasmBroker,
        client_id: &str,
        clean_start: bool,
        session_expiry: u32,
        will: Option<WillMessage>,
    ) -> Self {
        Self::connect_with_keep_alive(broker, client_id, clean_start, session_expiry, will, 60)
            .await
    }

    async fn connect_with_keep_alive(
        broker: &WasmBroker,
        client_id: &str,
        clean_start: bool,
        session_expiry: u32,
        will: Option<WillMessage>,
        keep_alive_secs: u64,
    ) -> Self {
        let port = broker.create_client_port().unwrap();
        let inbox = Rc::new(RefCell::new(Vec::new()));
        let inbox_in = Rc::clone(&inbox);
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let data = js_sys::Uint8Array::new(&event.data());
            inbox_in.borrow_mut().extend(data.to_vec());
        });
        port.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref())
            .unwrap();
        port.start();
        let client = Self {
            port,
            inbox,
            on_message,
        };

        let options = ConnectOptions::new(client_id)
            .with_clean_start(clean_start)
            .with_session_expiry_interval(session_expiry)
            .with_keep_alive(Duration::from_secs(keep_alive_secs));
        let options = match will {
            Some(will) => options.with_will(will),
            None => options,
        };
        client.send(&ConnectPacket::new(options));

        for _ in 0..200 {
            if client.inbox.borrow().len() >= 4 {
                break;
            }
            sleep(5).await;
        }
        let inbox = client.inbox.borrow().clone();
        assert!(inbox.len() >= 4, "no CONNACK for {client_id}");
        assert_eq!(inbox[0], 0x20, "expected CONNACK");
        assert_eq!(inbox[3], 0x00, "CONNACK reason must be Success");
        client
    }

    fn send(&self, packet: &impl MqttPacket) {
        let mut buf = BytesMut::new();
        packet.encode(&mut buf).unwrap();
        let array = js_sys::Uint8Array::from(&buf[..]);
        self.port.post_message(&array.buffer()).unwrap();
    }

    fn close_without_disconnect(self) {
        self.port.close();
    }

    fn disconnect(self, reason: ReasonCode) {
        self.send(&DisconnectPacket::new(reason));
        self.port.close();
    }
}

impl Drop for RawClient {
    fn drop(&mut self) {
        self.port
            .remove_event_listener_with_callback(
                "message",
                self.on_message.as_ref().unchecked_ref(),
            )
            .unwrap();
    }
}

async fn watch_will(broker: &WasmBroker, client_id: &str) -> (WasmMqttClient, Rc<Cell<usize>>) {
    let watcher = WasmMqttClient::new(format!("{client_id}-watcher"));
    watcher
        .connect_message_port_with_options(
            broker.create_client_port().unwrap(),
            &WasmConnectOptions::new(),
        )
        .await
        .unwrap();
    let count = Rc::new(Cell::new(0usize));
    let sink = Rc::clone(&count);
    let callback = Closure::<dyn FnMut(JsValue, JsValue, JsValue)>::new(
        move |_: JsValue, _: JsValue, _: JsValue| sink.set(sink.get() + 1),
    );
    watcher
        .subscribe_with_options(
            &will_topic(client_id),
            callback.into_js_value().unchecked_into(),
            &WasmSubscribeOptions::new(),
        )
        .await
        .unwrap();
    sleep(100).await;
    (watcher, count)
}

async fn wait_for_will(count: &Rc<Cell<usize>>, timeout_ms: i32) -> bool {
    let deadline = now_ms() + f64::from(timeout_ms);
    while now_ms() < deadline {
        if count.get() > 0 {
            return true;
        }
        sleep(20).await;
    }
    count.get() > 0
}

async fn reconnect_within_delay_cancels_will(client_id: &str, clean_start: bool) {
    let broker = broker();
    let (watcher, count) = watch_will(&broker, client_id).await;

    let first = RawClient::connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 1)),
    )
    .await;
    let dropped_at = now_ms();
    first.close_without_disconnect();
    sleep(300).await;

    let second = RawClient::connect(&broker, client_id, clean_start, SESSION_EXPIRY, None).await;

    sleep_until(dropped_at + 3000.0).await;
    assert_eq!(
        count.get(),
        0,
        "a reconnect within the Will Delay Interval must cancel the Will"
    );

    second.disconnect(ReasonCode::Success);
    watcher.disconnect().await.unwrap();
}

#[wasm_bindgen_test]
async fn will_published_after_delay_without_reconnect() {
    let broker = broker();
    let client_id = "wasm-wd-no-reconnect";
    let (watcher, count) = watch_will(&broker, client_id).await;

    let conn = RawClient::connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 2)),
    )
    .await;
    let dropped_at = now_ms();
    conn.close_without_disconnect();

    sleep_until(dropped_at + 1200.0).await;
    assert_eq!(
        count.get(),
        0,
        "the Will must not be published before the Will Delay Interval"
    );
    assert!(
        wait_for_will(&count, 3000).await,
        "the Will must be published once the Will Delay Interval elapses"
    );
    sleep(300).await;
    assert_eq!(count.get(), 1, "the Will is published once");

    watcher.disconnect().await.unwrap();
}

#[wasm_bindgen_test]
async fn resume_within_delay_cancels_will() {
    reconnect_within_delay_cancels_will("wasm-wd-resume", false).await;
}

#[wasm_bindgen_test]
async fn clean_start_within_delay_cancels_will() {
    reconnect_within_delay_cancels_will("wasm-wd-clean", true).await;
}

#[wasm_bindgen_test]
async fn session_expiry_zero_publishes_will_immediately() {
    let broker = broker();
    let client_id = "wasm-wd-expiry-zero";
    let (watcher, count) = watch_will(&broker, client_id).await;

    let conn = RawClient::connect(
        &broker,
        client_id,
        true,
        0,
        Some(delayed_will(client_id, 5)),
    )
    .await;
    conn.close_without_disconnect();

    assert!(
        wait_for_will(&count, 1500).await,
        "the Session ends at disconnect, so the Will must not wait for the delay"
    );

    watcher.disconnect().await.unwrap();
}

#[wasm_bindgen_test]
async fn normal_disconnect_then_close_discards_will() {
    let broker = broker();
    let client_id = "wasm-wd-normal";
    let (watcher, count) = watch_will(&broker, client_id).await;

    let conn = RawClient::connect(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 0)),
    )
    .await;
    conn.disconnect(ReasonCode::Success);

    sleep(1500).await;
    assert_eq!(count.get(), 0, "DISCONNECT 0x00 must delete the Will");

    watcher.disconnect().await.unwrap();
}

#[wasm_bindgen_test]
async fn keep_alive_expiry_publishes_will() {
    let broker = broker();
    let client_id = "wasm-wd-keepalive";
    let (watcher, count) = watch_will(&broker, client_id).await;

    let silent = RawClient::connect_with_keep_alive(
        &broker,
        client_id,
        true,
        SESSION_EXPIRY,
        Some(delayed_will(client_id, 0)),
        1,
    )
    .await;

    assert!(
        wait_for_will(&count, 4000).await,
        "a client silent past 1.5x its Keep Alive is disconnected and its Will published"
    );

    drop(silent);
    watcher.disconnect().await.unwrap();
}
