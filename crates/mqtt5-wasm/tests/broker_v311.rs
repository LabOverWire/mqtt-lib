#![cfg(all(target_arch = "wasm32", feature = "broker"))]

use futures::future::{select, Either};
use mqtt5_wasm::{
    WasmBroker, WasmBrokerConfig, WasmConnectOptions, WasmMqttClient, WasmPublishOptions,
    WasmSubscribeOptions,
};
use std::cell::RefCell;
use std::future::Future;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::wasm_bindgen_test;

const PAYLOAD: [u8; 3] = [0x00, 0x01, 0x02];

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

async fn within<T>(future: impl Future<Output = T>) -> Option<T> {
    match select(Box::pin(future), Box::pin(sleep(1_000))).await {
        Either::Left((value, _)) => Some(value),
        Either::Right(_) => None,
    }
}

async fn connected(broker: &WasmBroker, client_id: &str, protocol_version: u8) -> WasmMqttClient {
    let client = WasmMqttClient::new(client_id.to_string());
    let mut options = WasmConnectOptions::new();
    options.set_protocol_version(protocol_version);
    client
        .connect_message_port_with_options(broker.create_client_port().unwrap(), &options)
        .await
        .unwrap();
    client
}

type Deliveries = Option<Vec<(u8, Vec<u8>)>>;

#[derive(Debug, PartialEq)]
struct RoundTrip {
    before_unsubscribe: Deliveries,
    unsubscribed: bool,
    after_unsubscribe: Deliveries,
    connected: bool,
}

async fn subscribed_deliveries(
    subscriber: &WasmMqttClient,
    publisher: &WasmMqttClient,
    topic: &str,
) -> Deliveries {
    let received = subscribe_collecting(subscriber, topic).await?;

    let mut delivered = Vec::new();
    for qos in [0u8, 1] {
        let mut publish_options = WasmPublishOptions::new();
        publish_options.set_qos(qos);
        within(publisher.publish_with_options(topic, &PAYLOAD, &publish_options)).await;
        for _ in 0..50 {
            if !received.borrow().is_empty() {
                break;
            }
            sleep(10).await;
        }
        delivered.extend(
            received
                .borrow_mut()
                .drain(..)
                .map(|payload| (qos, payload)),
        );
    }
    Some(delivered)
}

async fn round_trip(protocol_version: u8) -> RoundTrip {
    let mut config = WasmBrokerConfig::new();
    config.set_allow_anonymous(true);
    let broker = WasmBroker::with_config(config).unwrap();
    let subscriber = connected(&broker, "v311-sub", protocol_version).await;
    let publisher = connected(&broker, "v311-pub", protocol_version).await;

    let before_unsubscribe = subscribed_deliveries(&subscriber, &publisher, "t").await;
    let unsubscribed = matches!(within(subscriber.unsubscribe("t")).await, Some(Ok(_)));
    let after_unsubscribe = subscribed_deliveries(&subscriber, &publisher, "u").await;
    let outcome = RoundTrip {
        before_unsubscribe,
        unsubscribed,
        after_unsubscribe,
        connected: publisher.is_connected() && subscriber.is_connected(),
    };
    within(publisher.disconnect()).await;
    within(subscriber.disconnect()).await;
    outcome
}

fn served() -> RoundTrip {
    let both_qos = Some(vec![(0, PAYLOAD.to_vec()), (1, PAYLOAD.to_vec())]);
    RoundTrip {
        before_unsubscribe: both_qos.clone(),
        unsubscribed: true,
        after_unsubscribe: both_qos,
        connected: true,
    }
}

#[wasm_bindgen_test]
async fn v5_client_is_served_by_the_wasm_broker() {
    assert_eq!(round_trip(5).await, served());
}

#[wasm_bindgen_test]
async fn v311_client_is_served_by_the_wasm_broker() {
    assert_eq!(round_trip(4).await, served());
}

type Received = Rc<RefCell<Vec<Vec<u8>>>>;

async fn subscribe_collecting(subscriber: &WasmMqttClient, topic: &str) -> Option<Received> {
    let received = Rc::new(RefCell::new(Vec::<Vec<u8>>::new()));
    let sink = Rc::clone(&received);
    let callback = Closure::<dyn FnMut(JsValue, JsValue, JsValue)>::new(
        move |_: JsValue, payload: JsValue, _: JsValue| {
            sink.borrow_mut()
                .push(js_sys::Uint8Array::new(&payload).to_vec());
        },
    );
    let mut subscribe_options = WasmSubscribeOptions::new();
    subscribe_options.set_qos(1);
    within(subscriber.subscribe_with_options(
        topic,
        callback.into_js_value().unchecked_into(),
        &subscribe_options,
    ))
    .await?
    .ok()?;
    Some(received)
}

async fn publish_with_properties(publisher: &WasmMqttClient, topic: &str, qos: u8, retain: bool) {
    let mut publish_options = WasmPublishOptions::new();
    publish_options.set_qos(qos);
    publish_options.set_retain(retain);
    publish_options.set_payload_format_indicator(Some(false));
    within(publisher.publish_with_options(topic, &PAYLOAD, &publish_options))
        .await
        .expect("PUBACK in time")
        .expect("publish");
}

async fn first_delivery(received: &Received) -> Option<Vec<u8>> {
    for _ in 0..100 {
        if let Some(payload) = received.borrow().first() {
            return Some(payload.clone());
        }
        sleep(10).await;
    }
    None
}

async fn retained_payload_at(protocol_version: u8) -> Option<Vec<u8>> {
    let mut config = WasmBrokerConfig::new();
    config.set_allow_anonymous(true);
    let broker = WasmBroker::with_config(config).unwrap();
    let publisher = connected(&broker, "v5-pub", 5).await;
    publish_with_properties(&publisher, "retained", 0, true).await;
    sleep(50).await;
    let subscriber = connected(&broker, "retained-sub", protocol_version).await;
    let received = subscribe_collecting(&subscriber, "retained")
        .await
        .expect("subscribed");
    let payload = first_delivery(&received).await;
    within(publisher.disconnect()).await;
    within(subscriber.disconnect()).await;
    payload
}

async fn queued_payload_at(protocol_version: u8) -> Option<Vec<u8>> {
    let mut config = WasmBrokerConfig::new();
    config.set_allow_anonymous(true);
    let broker = WasmBroker::with_config(config).unwrap();
    let subscriber = WasmMqttClient::new("queued-sub".to_string());
    let mut options = WasmConnectOptions::new();
    options.set_protocol_version(protocol_version);
    options.set_clean_start(false);
    options.set_session_expiry_interval(Some(300));
    subscriber
        .connect_message_port_with_options(broker.create_client_port().unwrap(), &options)
        .await
        .unwrap();
    let received = subscribe_collecting(&subscriber, "queued")
        .await
        .expect("subscribed");
    within(subscriber.disconnect()).await;
    sleep(50).await;
    let publisher = connected(&broker, "v5-pub", 5).await;
    publish_with_properties(&publisher, "queued", 1, false).await;
    subscriber
        .connect_message_port_with_options(broker.create_client_port().unwrap(), &options)
        .await
        .unwrap();
    let payload = first_delivery(&received).await;
    within(publisher.disconnect()).await;
    within(subscriber.disconnect()).await;
    payload
}

#[wasm_bindgen_test]
async fn retained_delivery_to_v5_subscriber_carries_the_published_payload() {
    assert_eq!(retained_payload_at(5).await, Some(PAYLOAD.to_vec()));
}

#[wasm_bindgen_test]
async fn retained_delivery_to_v311_subscriber_carries_the_published_payload() {
    assert_eq!(retained_payload_at(4).await, Some(PAYLOAD.to_vec()));
}

#[wasm_bindgen_test]
async fn queued_delivery_to_v5_subscriber_carries_the_published_payload() {
    assert_eq!(queued_payload_at(5).await, Some(PAYLOAD.to_vec()));
}

#[wasm_bindgen_test]
async fn queued_delivery_to_v311_subscriber_carries_the_published_payload() {
    assert_eq!(queued_payload_at(4).await, Some(PAYLOAD.to_vec()));
}
