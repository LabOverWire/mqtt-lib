#![cfg(target_arch = "wasm32")]

use bytes::BytesMut;
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::puback::PubAckPacket;
use mqtt5_protocol::packet::pubcomp::PubCompPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::pubrec::PubRecPacket;
use mqtt5_protocol::packet::pubrel::PubRelPacket;
use mqtt5_protocol::packet::suback::SubAckPacket;
use mqtt5_protocol::packet::{FixedHeader, MqttPacket, Packet};
use mqtt5_protocol::protocol::v5::properties::{PropertyId, PropertyValue};
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::QoS;
use mqtt5_wasm::{WasmConnectOptions, WasmMqttClient, WasmPublishOptions, WasmSubscribeOptions};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use wasm_bindgen_test::wasm_bindgen_test;
use web_sys::{Event, MessageChannel, MessageEvent, MessagePort};

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

fn encode(packet: &impl MqttPacket) -> Vec<u8> {
    let mut buf = BytesMut::new();
    packet.encode(&mut buf).unwrap();
    buf.to_vec()
}

struct Frame {
    first_byte: u8,
    packet: Packet,
}

fn take_frame(inbox: &mut Vec<u8>) -> Option<Frame> {
    let mut cursor = &inbox[..];
    let header = FixedHeader::decode(&mut cursor).ok()?;
    let header_len = inbox.len() - cursor.len();
    let total = header_len + header.remaining_length as usize;
    if inbox.len() < total {
        return None;
    }
    let first_byte = inbox[0];
    let mut body = &inbox[header_len..total];
    let packet = Packet::decode_from_body(header.packet_type, &header, &mut body).unwrap();
    inbox.drain(..total);
    Some(Frame { first_byte, packet })
}

struct FakeBroker {
    port: MessagePort,
    inbox: Rc<RefCell<Vec<u8>>>,
    closed: Rc<Cell<bool>>,
    on_message: Closure<dyn FnMut(MessageEvent)>,
    on_close: Closure<dyn FnMut(Event)>,
}

impl FakeBroker {
    fn new() -> (Self, MessagePort) {
        let channel = MessageChannel::new().unwrap();
        let port = channel.port1();
        let inbox = Rc::new(RefCell::new(Vec::new()));
        let closed = Rc::new(Cell::new(false));
        let inbox_in = Rc::clone(&inbox);
        let on_message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let data = js_sys::Uint8Array::new(&event.data());
            inbox_in.borrow_mut().extend(data.to_vec());
        });
        port.add_event_listener_with_callback("message", on_message.as_ref().unchecked_ref())
            .unwrap();
        let closed_in = Rc::clone(&closed);
        let on_close = Closure::<dyn FnMut(Event)>::new(move |_: Event| closed_in.set(true));
        port.add_event_listener_with_callback("close", on_close.as_ref().unchecked_ref())
            .unwrap();
        port.start();
        (
            Self {
                port,
                inbox,
                closed,
                on_message,
                on_close,
            },
            channel.port2(),
        )
    }

    fn send_raw(&self, bytes: &[u8]) {
        let array = js_sys::Uint8Array::from(bytes);
        self.port.post_message(&array.buffer()).unwrap();
    }

    fn send(&self, packet: &impl MqttPacket) {
        self.send_raw(&encode(packet));
    }

    fn take_frame(&self) -> Option<Frame> {
        take_frame(&mut self.inbox.borrow_mut())
    }

    async fn next_frame(&self) -> Frame {
        for _ in 0..200 {
            if let Some(frame) = self.take_frame() {
                return frame;
            }
            sleep(5).await;
        }
        panic!("client sent no packet");
    }

    async fn next_packet(&self) -> Packet {
        self.next_frame().await.packet
    }

    async fn next_non_ping(&self) -> Frame {
        loop {
            let frame = self.next_frame().await;
            if !matches!(frame.packet, Packet::PingReq) {
                return frame;
            }
        }
    }

    async fn expect_silence(&self, ms: i32) {
        sleep(ms).await;
        while let Some(frame) = self.take_frame() {
            assert!(
                matches!(frame.packet, Packet::PingReq),
                "unexpected packet from client: {:?}",
                frame.packet
            );
        }
    }

    async fn wait_closed(&self) -> bool {
        for _ in 0..200 {
            if self.closed.get() {
                return true;
            }
            sleep(5).await;
        }
        false
    }

    async fn expect_disconnect(&self, reason: ReasonCode) {
        match self.next_non_ping().await.packet {
            Packet::Disconnect(disconnect) => assert_eq!(disconnect.reason_code, reason),
            other => panic!("expected DISCONNECT {reason:?}, got {other:?}"),
        }
        assert!(
            self.wait_closed().await,
            "client did not close the connection"
        );
    }

    async fn ack_subscribe(&self) {
        match self.next_non_ping().await.packet {
            Packet::Subscribe(subscribe) => {
                self.send(
                    &SubAckPacket::new(subscribe.packet_id).add_granted_qos(QoS::ExactlyOnce),
                );
            }
            other => panic!("expected SUBSCRIBE, got {other:?}"),
        }
    }
}

impl Drop for FakeBroker {
    fn drop(&mut self) {
        self.port
            .remove_event_listener_with_callback(
                "message",
                self.on_message.as_ref().unchecked_ref(),
            )
            .unwrap();
        self.port
            .remove_event_listener_with_callback("close", self.on_close.as_ref().unchecked_ref())
            .unwrap();
    }
}

type Outcome<T> = Rc<RefCell<Option<Result<T, JsValue>>>>;

fn spawn_outcome<T: 'static, F>(future: F) -> Outcome<T>
where
    F: std::future::Future<Output = Result<T, JsValue>> + 'static,
{
    let outcome: Outcome<T> = Rc::new(RefCell::new(None));
    let slot = Rc::clone(&outcome);
    spawn_local(async move {
        let result = future.await;
        *slot.borrow_mut() = Some(result);
    });
    outcome
}

async fn settle<T>(outcome: &Outcome<T>) -> Result<T, JsValue> {
    for _ in 0..400 {
        if let Some(result) = outcome.borrow_mut().take() {
            return result;
        }
        sleep(5).await;
    }
    panic!("operation did not settle");
}

fn is_pending<T>(outcome: &Outcome<T>) -> bool {
    outcome.borrow().is_none()
}

async fn assert_rejected_silently<T: std::fmt::Debug>(outcome: &Outcome<T>, broker: &FakeBroker) {
    broker.expect_silence(60).await;
    let result = settle(outcome).await;
    assert!(
        result.is_err(),
        "operation should be rejected, got {result:?}"
    );
}

fn noop() -> js_sys::Function {
    js_sys::Function::new_no_args("")
}

fn recorder() -> (js_sys::Function, Rc<RefCell<Vec<String>>>) {
    let topics = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&topics);
    let callback = Closure::<dyn FnMut(JsValue, JsValue, JsValue)>::new(
        move |topic: JsValue, _payload: JsValue, _props: JsValue| {
            sink.borrow_mut()
                .push(topic.as_string().unwrap_or_default());
        },
    );
    (callback.into_js_value().unchecked_into(), topics)
}

fn value_recorder() -> (js_sys::Function, Rc<RefCell<Vec<JsValue>>>) {
    let values = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&values);
    let callback = Closure::<dyn FnMut(JsValue)>::new(move |value: JsValue| {
        sink.borrow_mut().push(value);
    });
    (callback.into_js_value().unchecked_into(), values)
}

fn success() -> ConnAckPacket {
    ConnAckPacket::new(false, ReasonCode::Success)
}

async fn open_session(
    client: &Rc<WasmMqttClient>,
    options: WasmConnectOptions,
    connack: ConnAckPacket,
) -> (FakeBroker, Result<(), JsValue>, Packet) {
    let (broker, client_port) = FakeBroker::new();
    let connecting = {
        let client = Rc::clone(client);
        spawn_outcome(async move {
            client
                .connect_message_port_with_options(client_port, &options)
                .await
        })
    };
    let connect = broker.next_packet().await;
    assert!(
        matches!(connect, Packet::Connect(_)),
        "expected CONNECT, got {connect:?}"
    );
    broker.send(&connack);
    let result = settle(&connecting).await;
    (broker, result, connect)
}

async fn connect_with(
    options: WasmConnectOptions,
    connack: ConnAckPacket,
) -> (Rc<WasmMqttClient>, FakeBroker) {
    let client = Rc::new(WasmMqttClient::new("wasm-conformance".to_string()));
    let (broker, result, _) = open_session(&client, options, connack).await;
    result.expect("connect failed");
    (client, broker)
}

async fn connect_default() -> (Rc<WasmMqttClient>, FakeBroker) {
    connect_with(WasmConnectOptions::new(), success()).await
}

fn publish_options(qos: u8) -> WasmPublishOptions {
    let mut options = WasmPublishOptions::new();
    options.set_qos(qos);
    options
}

fn publish_with(
    client: &Rc<WasmMqttClient>,
    topic: &str,
    payload: &[u8],
    options: WasmPublishOptions,
) -> Outcome<()> {
    let client = Rc::clone(client);
    let topic = topic.to_string();
    let payload = payload.to_vec();
    spawn_outcome(async move {
        client
            .publish_with_options(&topic, &payload, &options)
            .await
    })
}

fn subscribe_with(
    client: &Rc<WasmMqttClient>,
    filter: &str,
    options: WasmSubscribeOptions,
) -> Outcome<u16> {
    let client = Rc::clone(client);
    let filter = filter.to_string();
    spawn_outcome(async move {
        client
            .subscribe_with_options(&filter, noop(), &options)
            .await
    })
}

fn inbound_publish(topic: &str, qos: QoS, packet_id: Option<u16>) -> PublishPacket {
    let packet = PublishPacket::new(topic.to_string(), b"payload".to_vec(), qos);
    match packet_id {
        Some(id) => packet.with_packet_id(id),
        None => packet,
    }
}

async fn subscribed_client(
    options: WasmConnectOptions,
) -> (Rc<WasmMqttClient>, FakeBroker, Rc<RefCell<Vec<String>>>) {
    let (client, broker) = connect_with(options, success()).await;
    let (callback, topics) = recorder();
    client.subscribe_with_callback("#", callback).await.unwrap();
    broker.ack_subscribe().await;
    (client, broker, topics)
}

async fn wait_for_count<T>(items: &Rc<RefCell<Vec<T>>>, count: usize) {
    for _ in 0..200 {
        if items.borrow().len() >= count {
            return;
        }
        sleep(5).await;
    }
}

#[wasm_bindgen_test]
async fn mqtt_4_7_0_1_publish_topic_with_wildcard_rejected() {
    let (client, broker) = connect_default().await;
    assert!(client.publish("a/+", b"x").await.is_err());
    assert!(client.publish("a/#", b"x").await.is_err());
    let outcome = publish_with(&client, "a/+/b", b"x", publish_options(1));
    assert_rejected_silently(&outcome, &broker).await;
    assert!(client.publish_qos1("#", b"x", noop()).await.is_err());
    assert!(client.publish_qos2("+", b"x", noop()).await.is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn mqtt_4_7_3_1_empty_topic_without_alias_rejected() {
    let (client, broker) = connect_default().await;
    assert!(client.publish("", b"x").await.is_err());
    let outcome = publish_with(&client, "", b"x", publish_options(1));
    assert_rejected_silently(&outcome, &broker).await;
}

#[wasm_bindgen_test]
async fn mqtt_4_7_3_1_empty_topic_with_unmapped_alias_rejected() {
    let (client, broker) = connect_with(
        WasmConnectOptions::new(),
        success().with_topic_alias_maximum(5),
    )
    .await;
    let mut options = publish_options(0);
    options.set_topic_alias(Some(2));
    let outcome = publish_with(&client, "", b"x", options);
    assert_rejected_silently(&outcome, &broker).await;
}

#[wasm_bindgen_test]
async fn mqtt_4_7_3_2_publish_topic_with_null_rejected() {
    let (client, broker) = connect_default().await;
    assert!(client.publish("a\0b", b"x").await.is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_3_2_14_wildcard_response_topic_rejected() {
    let (client, broker) = connect_default().await;
    let mut options = publish_options(0);
    options.set_response_topic(Some("reply/#".to_string()));
    let outcome = publish_with(&client, "a", b"x", options);
    assert_rejected_silently(&outcome, &broker).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_3_2_8_topic_alias_zero_rejected() {
    let (client, broker) = connect_with(
        WasmConnectOptions::new(),
        success().with_topic_alias_maximum(5),
    )
    .await;
    let mut options = publish_options(0);
    options.set_topic_alias(Some(0));
    let outcome = publish_with(&client, "a", b"x", options);
    assert_rejected_silently(&outcome, &broker).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_18_topic_alias_without_server_maximum_rejected() {
    let (client, broker) = connect_default().await;
    let mut options = publish_options(0);
    options.set_topic_alias(Some(1));
    let outcome = publish_with(&client, "a", b"x", options);
    assert_rejected_silently(&outcome, &broker).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_3_2_9_topic_alias_above_server_maximum_rejected() {
    let (client, broker) = connect_with(
        WasmConnectOptions::new(),
        success().with_topic_alias_maximum(2),
    )
    .await;
    let mut above = publish_options(0);
    above.set_topic_alias(Some(3));
    let outcome = publish_with(&client, "a", b"x", above);
    assert_rejected_silently(&outcome, &broker).await;

    let mut within = publish_options(0);
    within.set_topic_alias(Some(2));
    settle(&publish_with(&client, "a", b"x", within))
        .await
        .unwrap();
    match broker.next_non_ping().await.packet {
        Packet::Publish(publish) => assert_eq!(publish.topic_alias(), Some(2)),
        other => panic!("expected PUBLISH, got {other:?}"),
    }
    let mut reuse = publish_options(0);
    reuse.set_topic_alias(Some(2));
    settle(&publish_with(&client, "", b"y", reuse))
        .await
        .unwrap();
    match broker.next_non_ping().await.packet {
        Packet::Publish(publish) => {
            assert_eq!(publish.topic_name, "");
            assert_eq!(publish.topic_alias(), Some(2));
        }
        other => panic!("expected PUBLISH, got {other:?}"),
    }
}

#[wasm_bindgen_test]
async fn mqtt_4_7_1_1_invalid_multi_level_wildcard_filter_rejected() {
    let (client, broker) = connect_default().await;
    assert!(client.subscribe("a/#/b").await.is_err());
    assert!(client.subscribe_with_callback("a#", noop()).await.is_err());
    let outcome = subscribe_with(&client, "sport/#/x", WasmSubscribeOptions::new());
    assert_rejected_silently(&outcome, &broker).await;
    assert!(client.unsubscribe("a/#/b").await.is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn mqtt_4_7_1_2_partial_level_single_wildcard_rejected() {
    let (client, broker) = connect_default().await;
    assert!(client.subscribe("a+/b").await.is_err());
    assert!(client.unsubscribe("a/b+").await.is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn mqtt_4_7_3_1_empty_filter_rejected() {
    let (client, broker) = connect_default().await;
    assert!(client.subscribe("").await.is_err());
    assert!(client.unsubscribe("").await.is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn mqtt_4_8_2_1_shared_subscription_without_share_name_rejected() {
    let (client, broker) = connect_default().await;
    assert!(client.subscribe("$share//a").await.is_err());
    assert!(client.subscribe("$share/group").await.is_err());
    assert!(client.subscribe("$share/group/").await.is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn mqtt_4_8_2_2_share_name_with_wildcard_rejected() {
    let (client, broker) = connect_default().await;
    assert!(client.subscribe("$share/g+/a").await.is_err());
    assert!(client.subscribe("$share/#/a").await.is_err());
    assert!(client.unsubscribe("$share/g#/a").await.is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_8_3_4_no_local_on_shared_subscription_rejected() {
    let (client, broker) = connect_default().await;
    let mut options = WasmSubscribeOptions::new();
    options.set_no_local(true);
    let outcome = subscribe_with(&client, "$share/g/a", options);
    assert_rejected_silently(&outcome, &broker).await;
}

#[wasm_bindgen_test]
async fn section_3_8_2_1_2_subscription_identifier_zero_rejected() {
    let (client, broker) = connect_default().await;
    let mut options = WasmSubscribeOptions::new();
    options.set_subscription_identifier(Some(0));
    let outcome = subscribe_with(&client, "a", options);
    assert_rejected_silently(&outcome, &broker).await;
}

#[wasm_bindgen_test]
async fn section_3_2_2_3_11_wildcard_subscription_unavailable_honoured() {
    let mut connack = success();
    connack
        .properties
        .add(
            PropertyId::WildcardSubscriptionAvailable,
            PropertyValue::Byte(0),
        )
        .unwrap();
    let (client, broker) = connect_with(WasmConnectOptions::new(), connack).await;
    assert!(client.subscribe("a/+").await.is_err());
    assert!(client.subscribe_with_callback("a/#", noop()).await.is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn section_3_2_2_3_13_shared_subscription_unavailable_honoured() {
    let mut connack = success();
    connack
        .properties
        .add(
            PropertyId::SharedSubscriptionAvailable,
            PropertyValue::Byte(0),
        )
        .unwrap();
    let (client, broker) = connect_with(WasmConnectOptions::new(), connack).await;
    assert!(client.subscribe("$share/g/a").await.is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn section_3_2_2_3_12_subscription_identifier_unavailable_honoured() {
    let mut connack = success();
    connack
        .properties
        .add(
            PropertyId::SubscriptionIdentifierAvailable,
            PropertyValue::Byte(0),
        )
        .unwrap();
    let (client, broker) = connect_with(WasmConnectOptions::new(), connack).await;
    let mut options = WasmSubscribeOptions::new();
    options.set_subscription_identifier(Some(7));
    let outcome = subscribe_with(&client, "a", options);
    assert_rejected_silently(&outcome, &broker).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_14_retain_unavailable_honoured() {
    let (client, broker) = connect_with(
        WasmConnectOptions::new(),
        success().with_retain_available(false),
    )
    .await;
    let mut options = publish_options(0);
    options.set_retain(true);
    let outcome = publish_with(&client, "a", b"x", options);
    assert_rejected_silently(&outcome, &broker).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_14_capabilities_refreshed_on_reconnect() {
    let client = Rc::new(WasmMqttClient::new("refresh".to_string()));
    let (broker, result, _) = open_session(
        &client,
        WasmConnectOptions::new(),
        success().with_retain_available(false),
    )
    .await;
    result.unwrap();
    let mut retained = publish_options(0);
    retained.set_retain(true);
    let outcome = publish_with(&client, "a", b"x", retained);
    assert_rejected_silently(&outcome, &broker).await;
    client.disconnect().await.unwrap();

    let (broker, result, _) = open_session(&client, WasmConnectOptions::new(), success()).await;
    result.unwrap();
    let mut retained = publish_options(0);
    retained.set_retain(true);
    settle(&publish_with(&client, "a", b"x", retained))
        .await
        .unwrap();
    match broker.next_non_ping().await.packet {
        Packet::Publish(publish) => assert!(publish.retain),
        other => panic!("expected PUBLISH, got {other:?}"),
    }
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_11_maximum_qos_honoured() {
    let (client, broker) =
        connect_with(WasmConnectOptions::new(), success().with_maximum_qos(0)).await;
    let outcome = publish_with(&client, "a", b"x", publish_options(1));
    let qos1 = client.publish_qos1("a", b"x", noop()).await;
    let qos2 = client.publish_qos2("a", b"x", noop()).await;
    sleep(60).await;
    while let Some(frame) = broker.take_frame() {
        if let Packet::Publish(publish) = frame.packet {
            assert_eq!(publish.qos, QoS::AtMostOnce, "PUBLISH exceeds Maximum QoS");
        }
    }
    assert!(settle(&outcome).await.is_err());
    assert!(qos1.is_err());
    assert!(qos2.is_err());
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_15_server_maximum_packet_size_honoured() {
    let (client, broker) = connect_with(
        WasmConnectOptions::new(),
        success().with_maximum_packet_size(64),
    )
    .await;
    let big = vec![0u8; 200];
    assert!(client.publish("a", &big).await.is_err());
    let outcome = publish_with(&client, "a", &big, publish_options(1));
    assert_rejected_silently(&outcome, &broker).await;
    let long_filter = "f".repeat(100);
    assert!(client.subscribe(&long_filter).await.is_err());
    assert!(client.unsubscribe(&long_filter).await.is_err());
    broker.expect_silence(40).await;
    client.publish("a", b"small").await.unwrap();
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Publish(_)
    ));
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_21_server_keep_alive_used() {
    let mut options = WasmConnectOptions::new();
    options.set_keep_alive(60);
    let (client, broker) = connect_with(options, success().with_server_keep_alive(1)).await;
    let mut pinged = false;
    for _ in 0..30 {
        sleep(50).await;
        if let Some(frame) = broker.take_frame() {
            pinged = matches!(frame.packet, Packet::PingReq);
            break;
        }
    }
    client.disconnect().await.unwrap();
    assert!(pinged, "client ignored the Server Keep Alive");
}

#[wasm_bindgen_test]
async fn section_3_1_2_10_keep_alive_zero_sends_no_pingreq() {
    let mut options = WasmConnectOptions::new();
    options.set_keep_alive(0);
    let (client, broker) = connect_with(options, success()).await;
    sleep(100).await;
    assert!(
        broker.take_frame().is_none(),
        "keep alive 0 must not trigger PINGREQ"
    );
    assert!(client.is_connected());
    client.disconnect().await.unwrap();
}

#[wasm_bindgen_test]
async fn mqtt_3_1_3_2_assigned_client_identifier_used_for_session() {
    let client = Rc::new(WasmMqttClient::new(String::new()));
    let mut connack = success();
    connack
        .properties
        .add(
            PropertyId::AssignedClientIdentifier,
            PropertyValue::Utf8String("server-assigned".to_string()),
        )
        .unwrap();
    let mut options = WasmConnectOptions::new();
    options.set_clean_start(false);
    options.set_session_expiry_interval(Some(300));
    let (broker, result, _) = open_session(&client, options, connack).await;
    result.unwrap();
    client.disconnect().await.unwrap();
    assert!(matches!(broker.next_packet().await, Packet::Disconnect(_)));

    let mut options = WasmConnectOptions::new();
    options.set_clean_start(false);
    options.set_session_expiry_interval(Some(300));
    let (_broker, result, connect) = open_session(
        &client,
        options,
        ConnAckPacket::new(true, ReasonCode::Success),
    )
    .await;
    result.unwrap();
    match connect {
        Packet::Connect(connect) => assert_eq!(connect.client_id, "server-assigned"),
        other => panic!("expected CONNECT, got {other:?}"),
    }
}

#[wasm_bindgen_test]
async fn section_3_1_2_11_connect_carries_requested_properties() {
    let client = Rc::new(WasmMqttClient::new("props".to_string()));
    let mut options = WasmConnectOptions::new();
    options.set_request_problem_information(Some(false));
    options.set_request_response_information(Some(true));
    options.add_user_property("k".to_string(), "v".to_string());
    let (_broker, result, connect) = open_session(&client, options, success()).await;
    result.unwrap();
    match connect {
        Packet::Connect(connect) => {
            assert_eq!(
                connect.properties.get_request_problem_information(),
                Some(false)
            );
            assert_eq!(
                connect.properties.get_request_response_information(),
                Some(true)
            );
            assert!(connect.properties.contains(PropertyId::UserProperty));
        }
        other => panic!("expected CONNECT, got {other:?}"),
    }
}

#[wasm_bindgen_test]
async fn mqtt_4_12_0_7_no_auth_without_authentication_method() {
    let client = Rc::new(WasmMqttClient::new("auth".to_string()));
    let mut with_method = WasmConnectOptions::new();
    with_method.set_authentication_method(Some("SCRAM".to_string()));
    let (broker, result, _) = open_session(
        &client,
        with_method,
        success().with_authentication_method("SCRAM".to_string()),
    )
    .await;
    result.unwrap();
    client.disconnect().await.unwrap();
    assert!(matches!(broker.next_packet().await, Packet::Disconnect(_)));

    let (broker, result, _) = open_session(&client, WasmConnectOptions::new(), success()).await;
    result.unwrap();
    assert!(client.respond_auth(b"data").is_err());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn section_4_12_unexpected_auth_is_protocol_error() {
    let (_client, broker) = connect_default().await;
    let mut auth = AuthPacket::new(ReasonCode::ContinueAuthentication);
    auth.properties
        .set_authentication_method("SCRAM".to_string());
    broker.send(&auth);
    broker.expect_disconnect(ReasonCode::ProtocolError).await;
}

#[wasm_bindgen_test]
async fn mqtt_2_2_1_3_packet_identifier_not_reused_while_in_flight() {
    let (client, broker) = connect_default().await;
    let held = client.publish_qos1("held", b"x", noop()).await.unwrap();
    assert!(matches!(broker.next_packet().await, Packet::Publish(_)));
    let mut issued = 0u32;
    while issued < 65_600 {
        let mut ids = Vec::with_capacity(2000);
        for _ in 0..2000 {
            let id = client.publish_qos1("t", b"", noop()).await.unwrap();
            assert_ne!(id, held, "packet identifier {held} reused while in flight");
            ids.push(id);
        }
        issued += 2000;
        let mut acks = Vec::with_capacity(ids.len() * 4);
        for _ in &ids {
            let frame = broker.next_frame().await;
            match frame.packet {
                Packet::Publish(publish) => {
                    acks.extend(encode(&PubAckPacket::new(publish.packet_id.unwrap())));
                }
                other => panic!("expected PUBLISH, got {other:?}"),
            }
        }
        for chunk in acks.chunks(4) {
            broker.send_raw(chunk);
        }
        sleep(20).await;
    }
}

#[wasm_bindgen_test]
async fn mqtt_4_4_0_1_unacknowledged_messages_resent_on_session_resume() {
    let client = Rc::new(WasmMqttClient::new("resume".to_string()));
    let mut options = WasmConnectOptions::new();
    options.set_clean_start(false);
    options.set_session_expiry_interval(Some(3600));
    let (broker, result, _) = open_session(&client, options, success()).await;
    result.unwrap();

    let first = publish_with(&client, "a", b"1", publish_options(1));
    let second = publish_with(&client, "b", b"2", publish_options(2));
    let third = publish_with(&client, "c", b"3", publish_options(1));
    let mut ids = Vec::new();
    for _ in 0..3 {
        match broker.next_non_ping().await.packet {
            Packet::Publish(publish) => ids.push(publish.packet_id.unwrap()),
            other => panic!("expected PUBLISH, got {other:?}"),
        }
    }
    broker.send(&PubRecPacket::new(ids[1]));
    match broker.next_non_ping().await.packet {
        Packet::PubRel(pubrel) => assert_eq!(pubrel.packet_id, ids[1]),
        other => panic!("expected PUBREL, got {other:?}"),
    }
    broker.send(&DisconnectPacket::new(ReasonCode::ServerShuttingDown));
    sleep(50).await;
    assert!(!client.is_connected());

    let mut options = WasmConnectOptions::new();
    options.set_clean_start(false);
    options.set_session_expiry_interval(Some(3600));
    let (broker, result, _) = open_session(
        &client,
        options,
        ConnAckPacket::new(true, ReasonCode::Success),
    )
    .await;
    result.unwrap();

    let resent: Vec<Frame> = {
        let mut frames = Vec::new();
        for _ in 0..3 {
            frames.push(broker.next_non_ping().await);
        }
        frames
    };
    match &resent[0].packet {
        Packet::Publish(publish) => {
            assert_eq!(publish.packet_id, Some(ids[0]));
            assert!(publish.dup, "resent PUBLISH must have DUP=1");
            assert_eq!(publish.topic_name, "a");
        }
        other => panic!("expected resent PUBLISH a, got {other:?}"),
    }
    match &resent[1].packet {
        Packet::PubRel(pubrel) => assert_eq!(pubrel.packet_id, ids[1]),
        other => panic!("expected resent PUBREL, got {other:?}"),
    }
    match &resent[2].packet {
        Packet::Publish(publish) => {
            assert_eq!(publish.packet_id, Some(ids[2]));
            assert_eq!(resent[2].first_byte & 0x08, 0x08);
            assert_eq!(publish.topic_name, "c");
        }
        other => panic!("expected resent PUBLISH c, got {other:?}"),
    }
    broker.send(&PubAckPacket::new(ids[0]));
    broker.send(&PubCompPacket::new(ids[1]));
    broker.send(&PubAckPacket::new(ids[2]));
    settle(&first).await.unwrap();
    settle(&second).await.unwrap();
    settle(&third).await.unwrap();
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_4_session_present_without_session_state_closes() {
    let client = Rc::new(WasmMqttClient::new("fresh".to_string()));
    let (broker, result, _) = open_session(
        &client,
        WasmConnectOptions::new(),
        ConnAckPacket::new(true, ReasonCode::Success),
    )
    .await;
    assert!(
        result.is_err(),
        "Session Present=1 without session state must fail"
    );
    assert!(
        broker.wait_closed().await,
        "client did not close the connection"
    );
    assert!(!client.is_connected());
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_4_fresh_client_clean_start_zero_session_present_closes() {
    let client = Rc::new(WasmMqttClient::new("fresh-resume".to_string()));
    let mut options = WasmConnectOptions::new();
    options.set_clean_start(false);
    options.set_session_expiry_interval(Some(3600));
    let (broker, result, _) = open_session(
        &client,
        options,
        ConnAckPacket::new(true, ReasonCode::Success),
    )
    .await;
    assert!(
        result.is_err(),
        "Session Present=1 without session state must fail"
    );
    assert!(
        broker.wait_closed().await,
        "client did not close the connection"
    );
    assert!(!client.is_connected());
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_4_resume_existing_session_opt_in_accepts_session_present() {
    let client = Rc::new(WasmMqttClient::new("opt-in-resume".to_string()));
    let mut options = WasmConnectOptions::new();
    options.set_clean_start(false);
    options.set_session_expiry_interval(Some(3600));
    options.set_resume_existing_session(true);
    let (broker, result, _) = open_session(
        &client,
        options,
        ConnAckPacket::new(true, ReasonCode::Success),
    )
    .await;
    result.expect("opt-in resume must accept Session Present=1");
    assert!(client.is_connected());
    broker.expect_silence(60).await;
    assert!(!broker.closed.get());

    broker.send(&inbound_publish("held/topic", QoS::AtLeastOnce, Some(11)));
    match broker.next_non_ping().await.packet {
        Packet::PubAck(puback) => assert_eq!(puback.packet_id, 11),
        other => panic!("expected PUBACK, got {other:?}"),
    }
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_4_resume_existing_session_still_rejects_clean_start() {
    let client = Rc::new(WasmMqttClient::new("opt-in-clean".to_string()));
    let mut options = WasmConnectOptions::new();
    options.set_resume_existing_session(true);
    let (broker, result, _) = open_session(
        &client,
        options,
        ConnAckPacket::new(true, ReasonCode::Success),
    )
    .await;
    assert!(
        result.is_err(),
        "Session Present=1 after Clean Start=1 must fail"
    );
    assert!(
        broker.wait_closed().await,
        "client did not close the connection"
    );
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_5_session_state_discarded_on_session_present_zero() {
    let client = Rc::new(WasmMqttClient::new("discard".to_string()));
    let mut options = WasmConnectOptions::new();
    options.set_clean_start(false);
    options.set_session_expiry_interval(Some(3600));
    let (broker, result, _) = open_session(&client, options, success()).await;
    result.unwrap();
    let pending = publish_with(&client, "a", b"1", publish_options(1));
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Publish(_)
    ));
    broker.send(&DisconnectPacket::new(ReasonCode::ServerShuttingDown));
    sleep(50).await;

    let mut options = WasmConnectOptions::new();
    options.set_clean_start(false);
    options.set_session_expiry_interval(Some(3600));
    let (broker, result, _) = open_session(&client, options, success()).await;
    result.unwrap();
    broker.expect_silence(60).await;
    assert!(settle(&pending).await.is_err());
}

#[wasm_bindgen_test]
async fn mqtt_3_3_4_7_server_receive_maximum_enforced() {
    let (client, broker) =
        connect_with(WasmConnectOptions::new(), success().with_receive_maximum(1)).await;
    let first = publish_with(&client, "a", b"1", publish_options(1));
    let second = publish_with(&client, "b", b"2", publish_options(1));
    let first_id = match broker.next_non_ping().await.packet {
        Packet::Publish(publish) => publish.packet_id.unwrap(),
        other => panic!("expected PUBLISH, got {other:?}"),
    };
    broker.expect_silence(60).await;
    client.publish("qos0", b"allowed").await.unwrap();
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Publish(_)
    ));
    assert!(is_pending(&second));
    broker.send(&PubAckPacket::new(first_id));
    settle(&first).await.unwrap();
    let second_id = match broker.next_non_ping().await.packet {
        Packet::Publish(publish) => publish.packet_id.unwrap(),
        other => panic!("expected second PUBLISH, got {other:?}"),
    };
    broker.send(&PubAckPacket::new(second_id));
    settle(&second).await.unwrap();
}

#[wasm_bindgen_test]
async fn mqtt_3_3_4_8_non_publish_packets_not_delayed_by_quota() {
    let (client, broker) =
        connect_with(WasmConnectOptions::new(), success().with_receive_maximum(1)).await;
    let first = publish_with(&client, "a", b"1", publish_options(1));
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Publish(_)
    ));
    let second = publish_with(&client, "b", b"2", publish_options(1));
    client.subscribe("x").await.unwrap();
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Subscribe(_)
    ));
    assert!(is_pending(&first));
    assert!(is_pending(&second));
}

#[wasm_bindgen_test]
async fn mqtt_4_9_0_1_send_quota_reset_on_new_connection() {
    let client = Rc::new(WasmMqttClient::new("quota".to_string()));
    let (broker, result, _) = open_session(
        &client,
        WasmConnectOptions::new(),
        success().with_receive_maximum(1),
    )
    .await;
    result.unwrap();
    let stuck = publish_with(&client, "a", b"1", publish_options(1));
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Publish(_)
    ));
    broker.send(&DisconnectPacket::new(ReasonCode::ServerShuttingDown));
    sleep(50).await;
    assert!(settle(&stuck).await.is_err());

    let (broker, result, _) = open_session(
        &client,
        WasmConnectOptions::new(),
        success().with_receive_maximum(1),
    )
    .await;
    result.unwrap();
    let fresh = publish_with(&client, "b", b"2", publish_options(1));
    let id = match broker.next_non_ping().await.packet {
        Packet::Publish(publish) => publish.packet_id.unwrap(),
        other => panic!("expected PUBLISH, got {other:?}"),
    };
    broker.send(&PubAckPacket::new(id));
    settle(&fresh).await.unwrap();
}

#[wasm_bindgen_test]
async fn mqtt_4_3_2_4_puback_sent_for_inbound_qos1() {
    let (_client, broker, topics) = subscribed_client(WasmConnectOptions::new()).await;
    broker.send(&inbound_publish("a/b", QoS::AtLeastOnce, Some(7)));
    match broker.next_non_ping().await.packet {
        Packet::PubAck(puback) => assert_eq!(puback.packet_id, 7),
        other => panic!("expected PUBACK, got {other:?}"),
    }
    wait_for_count(&topics, 1).await;
    assert_eq!(topics.borrow().as_slice(), ["a/b"]);
}

#[wasm_bindgen_test]
async fn mqtt_4_6_0_2_pubacks_sent_in_receive_order() {
    let (_client, broker, _topics) = subscribed_client(WasmConnectOptions::new()).await;
    for id in [30, 10, 20] {
        broker.send(&inbound_publish("a", QoS::AtLeastOnce, Some(id)));
    }
    let mut acked = Vec::new();
    for _ in 0..3 {
        match broker.next_non_ping().await.packet {
            Packet::PubAck(puback) => acked.push(puback.packet_id),
            other => panic!("expected PUBACK, got {other:?}"),
        }
    }
    assert_eq!(acked, [30, 10, 20]);
}

#[wasm_bindgen_test]
async fn mqtt_4_6_0_3_pubrecs_sent_in_receive_order() {
    let (_client, broker, _topics) = subscribed_client(WasmConnectOptions::new()).await;
    for id in [9, 3, 6] {
        broker.send(&inbound_publish("a", QoS::ExactlyOnce, Some(id)));
    }
    let mut recorded = Vec::new();
    for _ in 0..3 {
        match broker.next_non_ping().await.packet {
            Packet::PubRec(pubrec) => recorded.push(pubrec.packet_id),
            other => panic!("expected PUBREC, got {other:?}"),
        }
    }
    assert_eq!(recorded, [9, 3, 6]);
}

#[wasm_bindgen_test]
async fn mqtt_4_3_3_11_pubcomp_for_pubrel_and_identifier_reusable() {
    let (_client, broker, topics) = subscribed_client(WasmConnectOptions::new()).await;
    broker.send(&inbound_publish("first", QoS::ExactlyOnce, Some(5)));
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::PubRec(_)
    ));
    broker.send(&inbound_publish("first", QoS::ExactlyOnce, Some(5)).with_dup(true));
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::PubRec(_)
    ));
    broker.send(&PubRelPacket::new(5));
    match broker.next_non_ping().await.packet {
        Packet::PubComp(pubcomp) => assert_eq!(pubcomp.packet_id, 5),
        other => panic!("expected PUBCOMP, got {other:?}"),
    }
    broker.send(&inbound_publish("second", QoS::ExactlyOnce, Some(5)));
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::PubRec(_)
    ));
    wait_for_count(&topics, 2).await;
    assert_eq!(topics.borrow().as_slice(), ["first", "second"]);
}

#[wasm_bindgen_test]
async fn mqtt_6_0_0_2_multiple_packets_in_one_frame() {
    let (_client, broker, topics) = subscribed_client(WasmConnectOptions::new()).await;
    let mut frame = vec![0xD0, 0x00];
    frame.extend(encode(&inbound_publish("one", QoS::AtMostOnce, None)));
    frame.extend(encode(&inbound_publish("two", QoS::AtMostOnce, None)));
    broker.send_raw(&frame);
    wait_for_count(&topics, 2).await;
    assert_eq!(topics.borrow().as_slice(), ["one", "two"]);
}

#[wasm_bindgen_test]
async fn mqtt_6_0_0_2_packet_split_across_frames() {
    let (_client, broker, topics) = subscribed_client(WasmConnectOptions::new()).await;
    let bytes = encode(&inbound_publish("split/topic", QoS::AtMostOnce, None));
    broker.send_raw(&bytes[..1]);
    sleep(10).await;
    broker.send_raw(&bytes[1..3]);
    sleep(10).await;
    broker.send_raw(&bytes[3..]);
    wait_for_count(&topics, 1).await;
    assert_eq!(topics.borrow().as_slice(), ["split/topic"]);
}

#[wasm_bindgen_test]
async fn mqtt_3_3_2_10_inbound_topic_alias_resolved() {
    let mut options = WasmConnectOptions::new();
    options.set_topic_alias_maximum(Some(5));
    let (_client, broker, topics) = subscribed_client(options).await;
    broker.send(&inbound_publish("alias/topic", QoS::AtMostOnce, None).with_topic_alias(5));
    broker.send(&inbound_publish("", QoS::AtMostOnce, None).with_topic_alias(5));
    wait_for_count(&topics, 2).await;
    assert_eq!(topics.borrow().as_slice(), ["alias/topic", "alias/topic"]);
}

#[wasm_bindgen_test]
async fn section_3_3_2_3_4_inbound_topic_alias_above_maximum_disconnects() {
    let mut options = WasmConnectOptions::new();
    options.set_topic_alias_maximum(Some(2));
    let (_client, broker, _topics) = subscribed_client(options).await;
    broker.send(&inbound_publish("a", QoS::AtMostOnce, None).with_topic_alias(3));
    broker
        .expect_disconnect(ReasonCode::TopicAliasInvalid)
        .await;
}

#[wasm_bindgen_test]
async fn section_3_3_2_3_4_inbound_topic_alias_zero_disconnects() {
    let mut options = WasmConnectOptions::new();
    options.set_topic_alias_maximum(Some(2));
    let (_client, broker, _topics) = subscribed_client(options).await;
    broker.send(&inbound_publish("a", QoS::AtMostOnce, None).with_topic_alias(0));
    broker
        .expect_disconnect(ReasonCode::TopicAliasInvalid)
        .await;
}

#[wasm_bindgen_test]
async fn section_3_3_2_3_4_inbound_unknown_alias_is_protocol_error() {
    let mut options = WasmConnectOptions::new();
    options.set_topic_alias_maximum(Some(2));
    let (_client, broker, _topics) = subscribed_client(options).await;
    broker.send(&inbound_publish("", QoS::AtMostOnce, None).with_topic_alias(1));
    broker.expect_disconnect(ReasonCode::ProtocolError).await;
}

#[wasm_bindgen_test]
async fn section_3_3_4_inbound_receive_maximum_exceeded_disconnects() {
    let mut options = WasmConnectOptions::new();
    options.set_receive_maximum(Some(1));
    let (_client, broker, _topics) = subscribed_client(options).await;
    broker.send(&inbound_publish("a", QoS::ExactlyOnce, Some(1)));
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::PubRec(_)
    ));
    broker.send(&inbound_publish("a", QoS::ExactlyOnce, Some(2)));
    broker
        .expect_disconnect(ReasonCode::ReceiveMaximumExceeded)
        .await;
}

#[wasm_bindgen_test]
async fn section_3_1_2_11_4_inbound_packet_above_maximum_size_disconnects() {
    let mut options = WasmConnectOptions::new();
    options.set_maximum_packet_size(Some(128));
    let (_client, broker, topics) = subscribed_client(options).await;
    let mut publish = inbound_publish("big", QoS::AtMostOnce, None);
    publish.payload = vec![7u8; 400].into();
    broker.send(&publish);
    broker.expect_disconnect(ReasonCode::PacketTooLarge).await;
    assert!(topics.borrow().is_empty());
}

#[wasm_bindgen_test]
async fn mqtt_3_6_1_1_pubrel_reserved_flags_malformed() {
    let (_client, broker, _topics) = subscribed_client(WasmConnectOptions::new()).await;
    broker.send(&inbound_publish("a", QoS::ExactlyOnce, Some(4)));
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::PubRec(_)
    ));
    broker.send_raw(&[0x60, 0x02, 0x00, 0x04]);
    broker.expect_disconnect(ReasonCode::MalformedPacket).await;
}

#[wasm_bindgen_test]
async fn mqtt_2_1_3_1_suback_reserved_flags_malformed() {
    let (client, broker) = connect_default().await;
    let id = client.subscribe("a").await.unwrap();
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Subscribe(_)
    ));
    let [id_high, id_low] = id.to_be_bytes();
    let bytes = [0x92, 0x04, id_high, id_low, 0x00, 0x00];
    broker.send_raw(&bytes);
    broker.expect_disconnect(ReasonCode::MalformedPacket).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_14_1_1_disconnect_reserved_flags_malformed() {
    let (_client, broker) = connect_default().await;
    broker.send_raw(&[0xE1, 0x00]);
    broker.expect_disconnect(ReasonCode::MalformedPacket).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_15_1_1_auth_reserved_flags_malformed() {
    let (_client, broker) = connect_default().await;
    broker.send_raw(&[0xF1, 0x00]);
    broker.expect_disconnect(ReasonCode::MalformedPacket).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_3_1_4_publish_qos_three_malformed() {
    let (_client, broker) = connect_default().await;
    broker.send_raw(&[0x36, 0x06, 0x00, 0x01, b'a', 0x00, 0x01, 0x00]);
    broker.expect_disconnect(ReasonCode::MalformedPacket).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_2_2_1_connack_reserved_flags_rejected() {
    let client = Rc::new(WasmMqttClient::new("flags".to_string()));
    let (broker, client_port) = FakeBroker::new();
    let options = WasmConnectOptions::new();
    let connecting = {
        let client = Rc::clone(&client);
        spawn_outcome(async move {
            client
                .connect_message_port_with_options(client_port, &options)
                .await
        })
    };
    assert!(matches!(broker.next_packet().await, Packet::Connect(_)));
    broker.send_raw(&[0x20, 0x03, 0x02, 0x00, 0x00]);
    assert!(settle(&connecting).await.is_err());
    assert!(
        broker.wait_closed().await,
        "client did not close the connection"
    );
}

#[wasm_bindgen_test]
async fn section_3_14_server_disconnect_closes_connection() {
    let (client, broker) = connect_default().await;
    broker.send(&DisconnectPacket::new(ReasonCode::ServerShuttingDown));
    assert!(
        broker.wait_closed().await,
        "client kept the connection open"
    );
    assert!(!client.is_connected());
    broker.expect_silence(40).await;
}

#[wasm_bindgen_test]
async fn section_3_1_2_11_7_problem_information_zero_enforced() {
    let mut options = WasmConnectOptions::new();
    options.set_request_problem_information(Some(false));
    let (client, broker) = connect_with(options, success()).await;
    let id = client.subscribe("a").await.unwrap();
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Subscribe(_)
    ));
    broker.send(
        &SubAckPacket::new(id)
            .add_granted_qos(QoS::AtMostOnce)
            .with_reason_string("not allowed".to_string()),
    );
    broker.expect_disconnect(ReasonCode::ProtocolError).await;
}

#[wasm_bindgen_test]
async fn mqtt_3_14_4_1_nothing_sent_after_disconnect() {
    let (client, broker) = connect_with(
        WasmConnectOptions::new(),
        success().with_server_keep_alive(1),
    )
    .await;
    client.disconnect().await.unwrap();
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Disconnect(_)
    ));
    assert!(client.publish("a", b"x").await.is_err());
    assert!(client.subscribe("a").await.is_err());
    assert!(client.respond_auth(b"x").is_err());
    sleep(700).await;
    assert!(
        broker.take_frame().is_none(),
        "client sent a packet after DISCONNECT"
    );
}

#[wasm_bindgen_test]
async fn mqtt_4_3_2_2_first_qos1_send_has_dup_zero() {
    let (client, broker) = connect_default().await;
    let outcome = publish_with(&client, "a", b"x", publish_options(1));
    let frame = broker.next_non_ping().await;
    assert_eq!(
        frame.first_byte & 0x08,
        0,
        "first transmission must have DUP=0"
    );
    if let Packet::Publish(publish) = frame.packet {
        broker.send(&PubAckPacket::new(publish.packet_id.unwrap()));
    }
    settle(&outcome).await.unwrap();
}

#[wasm_bindgen_test]
async fn section_3_4_puback_error_reason_completes_flow_without_retransmit() {
    let (client, broker) = connect_default().await;
    let (callback, results) = value_recorder();
    let id = client.publish_qos1("a", b"x", callback).await.unwrap();
    assert!(matches!(
        broker.next_non_ping().await.packet,
        Packet::Publish(_)
    ));
    broker.send(&PubAckPacket::new_with_reason(
        id,
        ReasonCode::NotAuthorized,
    ));
    wait_for_count(&results, 1).await;
    assert_eq!(results.borrow()[0].as_f64(), Some(f64::from(0x87_u8)));
    broker.expect_silence(60).await;
}

#[wasm_bindgen(inline_js = r#"
const net = process.getBuiltinModule('node:net');
const crypto = process.getBuiltinModule('node:crypto');
export class WsFake {
  constructor() { this.protocols = ''; this.opcodes = []; this.data = []; this.closed = false; this.sock = null; this.buf = Buffer.alloc(0); }
  start() {
    return new Promise((resolve) => {
      this.server = net.createServer((sock) => this.onConn(sock));
      this.server.listen(0, '127.0.0.1', () => resolve(this.server.address().port));
    });
  }
  onConn(sock) {
    this.sock = sock;
    let handshaken = false;
    sock.on('data', (chunk) => {
      this.buf = Buffer.concat([this.buf, chunk]);
      if (!handshaken) {
        const idx = this.buf.indexOf('\r\n\r\n');
        if (idx < 0) return;
        const head = this.buf.subarray(0, idx).toString();
        this.buf = this.buf.subarray(idx + 4);
        const key = /sec-websocket-key: *(.*)/i.exec(head)[1].trim();
        const proto = /sec-websocket-protocol: *(.*)/i.exec(head);
        this.protocols = proto ? proto[1].trim() : '';
        const accept = crypto.createHash('sha1').update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
        sock.write('HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ' + accept + '\r\n' + (proto ? 'Sec-WebSocket-Protocol: mqtt\r\n' : '') + '\r\n');
        handshaken = true;
      }
      this.parse();
    });
    sock.on('close', () => { this.closed = true; });
    sock.on('error', () => {});
  }
  parse() {
    while (this.buf.length >= 2) {
      const b0 = this.buf[0], b1 = this.buf[1];
      let len = b1 & 0x7f, off = 2;
      if (len === 126) { if (this.buf.length < 4) return; len = this.buf.readUInt16BE(2); off = 4; }
      else if (len === 127) { if (this.buf.length < 10) return; len = Number(this.buf.readBigUInt64BE(2)); off = 10; }
      const masked = (b1 & 0x80) !== 0;
      const maskOff = off;
      if (masked) off += 4;
      if (this.buf.length < off + len) return;
      const payload = Buffer.from(this.buf.subarray(off, off + len));
      if (masked) { const m = this.buf.subarray(maskOff, maskOff + 4); for (let i = 0; i < payload.length; i++) payload[i] ^= m[i % 4]; }
      this.buf = this.buf.subarray(off + len);
      const op = b0 & 0x0f;
      this.opcodes.push(op);
      if (op === 8) { this.closeReceived = true; try { this.sock.write(Buffer.from([0x88, 0])); this.sock.end(); } catch (e) {} }
      else if (op === 0 || op === 1 || op === 2) { for (const b of payload) this.data.push(b); }
    }
  }
  frame(op, payload) {
    const p = Buffer.from(payload);
    let hdr;
    if (p.length < 126) hdr = Buffer.from([0x80 | op, p.length]);
    else { hdr = Buffer.alloc(4); hdr[0] = 0x80 | op; hdr[1] = 126; hdr.writeUInt16BE(p.length, 2); }
    this.sock.write(Buffer.concat([hdr, p]));
  }
  sendBinary(bytes) { this.frame(2, bytes); }
  sendText(text) { this.frame(1, Buffer.from(text)); }
  takeData() { const d = Uint8Array.from(this.data); this.data = []; return d; }
  opcodeList() { return Uint8Array.from(this.opcodes); }
  protocolHeader() { return this.protocols; }
  isClosed() { return this.closed || !!this.closeReceived; }
  stop() { try { if (this.sock) this.sock.destroy(); } catch (e) {} this.server.close(); }
}
"#)]
extern "C" {
    type WsFake;
    #[wasm_bindgen(constructor)]
    fn new() -> WsFake;
    #[wasm_bindgen(method)]
    fn start(this: &WsFake) -> js_sys::Promise;
    #[wasm_bindgen(method, js_name = sendBinary)]
    fn send_binary(this: &WsFake, bytes: &[u8]);
    #[wasm_bindgen(method, js_name = sendText)]
    fn send_text(this: &WsFake, text: &str);
    #[wasm_bindgen(method, js_name = takeData)]
    fn take_data(this: &WsFake) -> Vec<u8>;
    #[wasm_bindgen(method, js_name = opcodeList)]
    fn opcode_list(this: &WsFake) -> Vec<u8>;
    #[wasm_bindgen(method, js_name = protocolHeader)]
    fn protocol_header(this: &WsFake) -> String;
    #[wasm_bindgen(method, js_name = isClosed)]
    fn is_closed(this: &WsFake) -> bool;
    #[wasm_bindgen(method)]
    fn stop(this: &WsFake);
}

struct WsBroker {
    server: WsFake,
    inbox: RefCell<Vec<u8>>,
}

impl WsBroker {
    async fn next_packet(&self) -> Packet {
        for _ in 0..400 {
            self.inbox.borrow_mut().extend(self.server.take_data());
            if let Some(frame) = take_frame(&mut self.inbox.borrow_mut()) {
                return frame.packet;
            }
            sleep(5).await;
        }
        panic!("client sent no packet over WebSocket");
    }

    async fn wait_closed(&self) -> bool {
        for _ in 0..400 {
            if self.server.is_closed() {
                return true;
            }
            sleep(5).await;
        }
        false
    }
}

async fn connect_ws() -> (Rc<WasmMqttClient>, WsBroker) {
    let server = WsFake::new();
    let port = JsFuture::from(server.start())
        .await
        .unwrap()
        .as_f64()
        .unwrap();
    let broker = WsBroker {
        server,
        inbox: RefCell::new(Vec::new()),
    };
    let client = Rc::new(WasmMqttClient::new("ws-client".to_string()));
    let url = format!("ws://127.0.0.1:{port}/mqtt");
    let connecting = {
        let client = Rc::clone(&client);
        spawn_outcome(async move { client.connect(&url).await })
    };
    assert!(matches!(broker.next_packet().await, Packet::Connect(_)));
    let connack = encode(&success());
    broker.server.send_binary(&connack[..2]);
    sleep(10).await;
    broker.server.send_binary(&connack[2..]);
    settle(&connecting).await.expect("WebSocket connect failed");
    (client, broker)
}

#[wasm_bindgen_test]
async fn mqtt_6_0_0_3_websocket_offers_mqtt_subprotocol() {
    let (client, broker) = connect_ws().await;
    let offered = broker.server.protocol_header();
    assert!(
        offered.split(',').any(|p| p.trim() == "mqtt"),
        "offered subprotocols: {offered:?}"
    );
    client.disconnect().await.unwrap();
    broker.server.stop();
}

#[wasm_bindgen_test]
async fn mqtt_6_0_0_1_websocket_sends_binary_frames_only() {
    let (client, broker) = connect_ws().await;
    client.publish("a", b"payload").await.unwrap();
    assert!(matches!(broker.next_packet().await, Packet::Publish(_)));
    let opcodes = broker.server.opcode_list();
    assert!(opcodes.iter().all(|op| *op == 2), "opcodes: {opcodes:?}");
    client.disconnect().await.unwrap();
    broker.server.stop();
}

#[wasm_bindgen_test]
async fn mqtt_6_0_0_1_websocket_text_frame_closes_connection() {
    let (client, broker) = connect_ws().await;
    broker.server.send_text("not mqtt");
    assert!(
        broker.wait_closed().await,
        "client kept the connection after a text frame"
    );
    sleep(20).await;
    assert!(!client.is_connected());
    broker.server.stop();
}

#[wasm_bindgen_test]
async fn mqtt_6_0_0_2_websocket_coalesced_and_split_packets() {
    let (client, broker) = connect_ws().await;
    let (callback, topics) = recorder();
    client.subscribe_with_callback("#", callback).await.unwrap();
    let id = match broker.next_packet().await {
        Packet::Subscribe(subscribe) => subscribe.packet_id,
        other => panic!("expected SUBSCRIBE, got {other:?}"),
    };
    let mut coalesced = encode(&SubAckPacket::new(id).add_granted_qos(QoS::AtMostOnce));
    coalesced.extend([0xD0, 0x00]);
    coalesced.extend(encode(&inbound_publish("one", QoS::AtMostOnce, None)));
    let split = encode(&inbound_publish("two", QoS::AtMostOnce, None));
    coalesced.extend(&split[..1]);
    broker.server.send_binary(&coalesced);
    sleep(10).await;
    broker.server.send_binary(&split[1..]);
    wait_for_count(&topics, 2).await;
    assert_eq!(topics.borrow().as_slice(), ["one", "two"]);
    client.disconnect().await.unwrap();
    broker.server.stop();
}
