use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::subscribe::SubscribePacket;
use mqtt5_protocol::packet::unsubscribe::UnsubscribePacket;
use mqtt5_protocol::packet::{MqttPacket, Packet};
use mqtt5_protocol::protocol::v5::properties::Properties;
use crate::transport::{WasmReader, WasmTransportType, WasmWriter};
use mqtt5_protocol::Transport;
use crate::decoder::read_packet;
use mqtt5_protocol::QoS;
use bytes::BytesMut;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::MessagePort;

async fn sleep_ms(millis: u32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let window = web_sys::window().expect("no global window");
        window
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis as i32)
            .expect("setTimeout failed");
    });
    JsFuture::from(promise).await.ok();
}

struct ClientState {
    client_id: String,
    writer: Option<Rc<RefCell<WasmWriter>>>,
    packet_id: u16,
    connected: bool,
    subscriptions: HashMap<String, js_sys::Function>,
    pending_subacks: HashMap<u16, js_sys::Function>,
    pending_pubacks: HashMap<u16, js_sys::Function>,
    pending_pubcomps: HashMap<u16, (js_sys::Function, f64)>,
    pending_pubrecs: HashMap<u16, f64>,
    received_qos2: HashMap<u16, f64>,
    keep_alive: u16,
    last_ping_sent: Option<f64>,
    last_pong_received: Option<f64>,
    on_connect: Option<js_sys::Function>,
    on_disconnect: Option<js_sys::Function>,
    on_error: Option<js_sys::Function>,
}

impl ClientState {
    fn new(client_id: String) -> Self {
        Self {
            client_id,
            writer: None,
            packet_id: 0,
            connected: false,
            subscriptions: HashMap::new(),
            pending_subacks: HashMap::new(),
            pending_pubacks: HashMap::new(),
            pending_pubcomps: HashMap::new(),
            pending_pubrecs: HashMap::new(),
            received_qos2: HashMap::new(),
            keep_alive: 60,
            last_ping_sent: None,
            last_pong_received: None,
            on_connect: None,
            on_disconnect: None,
            on_error: None,
        }
    }

    fn next_packet_id(&mut self) -> u16 {
        self.packet_id = self.packet_id.wrapping_add(1);
        if self.packet_id == 0 {
            self.packet_id = 1;
        }
        self.packet_id
    }
}

fn encode_packet(packet: &Packet, buf: &mut BytesMut) -> mqtt5_protocol::error::Result<()> {
    match packet {
        Packet::Connect(p) => p.encode(buf),
        Packet::Publish(p) => p.encode(buf),
        Packet::PubRec(p) => p.encode(buf),
        Packet::PubRel(p) => p.encode(buf),
        Packet::PubComp(p) => p.encode(buf),
        Packet::Subscribe(p) => p.encode(buf),
        Packet::PingReq => mqtt5_protocol::packet::pingreq::PingReqPacket::default().encode(buf),
        Packet::Disconnect(p) => p.encode(buf),
        Packet::Unsubscribe(p) => p.encode(buf),
        _ => Err(mqtt5_protocol::error::MqttError::ProtocolError(format!(
            "Encoding not yet implemented for packet type: {:?}",
            packet
        ))),
    }
}

#[wasm_bindgen]
pub struct WasmMqttClient {
    state: Rc<RefCell<ClientState>>,
}

impl WasmMqttClient {
    fn trigger_disconnect_callback(state: &Rc<RefCell<ClientState>>) {
        let callback = state.borrow().on_disconnect.clone();
        if let Some(callback) = callback {
            if let Err(e) = callback.call0(&JsValue::NULL) {
                web_sys::console::error_1(&format!("onDisconnect callback error: {:?}", e).into());
            }
        }
    }

    fn trigger_error_callback(state: &Rc<RefCell<ClientState>>, error_msg: &str) {
        let callback = state.borrow().on_error.clone();
        if let Some(callback) = callback {
            let error_js = JsValue::from_str(error_msg);
            if let Err(e) = callback.call1(&JsValue::NULL, &error_js) {
                web_sys::console::error_1(&format!("onError callback error: {:?}", e).into());
            }
        }
    }

    fn spawn_packet_reader(&self, mut reader: WasmReader) {
        let state = Rc::clone(&self.state);

        spawn_local(async move {
            web_sys::console::log_1(&"Packet reader task started".into());
            loop {
                web_sys::console::log_1(&"Packet reader: reading next packet...".into());
                let packet_result = read_packet(&mut reader).await;
                web_sys::console::log_1(&"Packet reader: read_packet() returned".into());

                match packet_result {
                    Ok(packet) => {
                        web_sys::console::log_1(
                            &format!("Packet reader: handling packet: {:?}", packet).into(),
                        );
                        Self::handle_incoming_packet(&state, packet);
                    }
                    Err(e) => {
                        let was_connected = loop {
                            match state.try_borrow_mut() {
                                Ok(mut state_ref) => {
                                    let connected = state_ref.connected;
                                    state_ref.connected = false;
                                    break connected;
                                }
                                Err(_) => {
                                    sleep_ms(10).await;
                                    continue;
                                }
                            }
                        };

                        if was_connected {
                            let error_msg = format!("Packet read error: {}", e);
                            web_sys::console::error_1(&error_msg.clone().into());
                            Self::trigger_error_callback(&state, &error_msg);
                            Self::trigger_disconnect_callback(&state);
                        } else {
                            web_sys::console::log_1(
                                &"Packet reader: connection closed (expected during disconnect)"
                                    .into(),
                            );
                        }
                        break;
                    }
                }
            }
            web_sys::console::log_1(&"Packet reader task exited".into());
        });
    }

    fn spawn_keepalive_task(&self) {
        let state = Rc::clone(&self.state);

        spawn_local(async move {
            loop {
                let (keep_alive_ms, connected) = {
                    match state.try_borrow() {
                        Ok(state_ref) => {
                            let ms = (state_ref.keep_alive as f64) * 1000.0;
                            let conn = state_ref.connected;
                            (ms, conn)
                        }
                        Err(_) => {
                            sleep_ms(100).await;
                            continue;
                        }
                    }
                };

                if !connected {
                    break;
                }

                let sleep_duration = (keep_alive_ms / 2.0) as u32;
                sleep_ms(sleep_duration).await;

                let should_disconnect = {
                    match state.try_borrow() {
                        Ok(state_ref) => {
                            if !state_ref.connected {
                                break;
                            }

                            let now = js_sys::Date::now();

                            if let Some(last_ping) = state_ref.last_ping_sent {
                                if let Some(last_pong) = state_ref.last_pong_received {
                                    if last_ping > last_pong
                                        && (now - last_ping) > keep_alive_ms * 1.5
                                    {
                                        web_sys::console::error_1(&"Keepalive timeout".into());
                                        true
                                    } else {
                                        false
                                    }
                                } else if (now - last_ping) > keep_alive_ms * 1.5 {
                                    web_sys::console::error_1(&"Keepalive timeout".into());
                                    true
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        }
                        Err(_) => {
                            sleep_ms(100).await;
                            continue;
                        }
                    }
                };

                if should_disconnect {
                    state.borrow_mut().connected = false;
                    Self::trigger_error_callback(&state, "Keepalive timeout");
                    Self::trigger_disconnect_callback(&state);
                    break;
                }

                let packet = Packet::PingReq;
                let mut buf = BytesMut::new();
                if let Err(e) = encode_packet(&packet, &mut buf) {
                    web_sys::console::error_1(&format!("Ping encode error: {}", e).into());
                    continue;
                }

                state.borrow_mut().last_ping_sent = Some(js_sys::Date::now());

                let writer_rc = {
                    let state_ref = state.borrow();
                    state_ref.writer.as_ref().map(Rc::clone)
                };

                match writer_rc {
                    Some(writer_rc) => match writer_rc.borrow_mut().write(&buf).await {
                        Ok(_) => {}
                        Err(e) => {
                            let error_msg = format!("Ping send error: {}", e);
                            web_sys::console::error_1(&error_msg.clone().into());
                            state.borrow_mut().connected = false;
                            Self::trigger_error_callback(&state, &error_msg);
                            Self::trigger_disconnect_callback(&state);
                            break;
                        }
                    },
                    None => {
                        break;
                    }
                }
            }
        });
    }

    fn spawn_qos2_cleanup_task(&self) {
        let state = Rc::clone(&self.state);

        spawn_local(async move {
            loop {
                sleep_ms(5000).await;

                let connected = match state.try_borrow() {
                    Ok(state_ref) => state_ref.connected,
                    Err(_) => {
                        continue;
                    }
                };

                if !connected {
                    break;
                }

                let now = js_sys::Date::now();
                let timeout_ms = 10000.0;
                let cleanup_ms = 30000.0;

                match state.try_borrow_mut() {
                    Ok(mut state_ref) => {
                        let mut timed_out_pubcomps = Vec::new();

                        for (packet_id, (callback, timestamp)) in state_ref.pending_pubcomps.iter()
                        {
                            if now - timestamp > timeout_ms {
                                timed_out_pubcomps.push((*packet_id, callback.clone()));
                            }
                        }

                        for (packet_id, callback) in timed_out_pubcomps {
                            state_ref.pending_pubcomps.remove(&packet_id);
                            web_sys::console::warn_1(
                                &format!("QoS 2 publish timeout for packet {}", packet_id).into(),
                            );
                            let error = JsValue::from_str("Timeout");
                            if let Err(e) = callback.call1(&JsValue::NULL, &error) {
                                web_sys::console::error_1(
                                    &format!("QoS 2 timeout callback error: {:?}", e).into(),
                                );
                            }
                        }

                        state_ref.pending_pubrecs.retain(|packet_id, timestamp| {
                            let should_keep = now - *timestamp <= cleanup_ms;
                            if !should_keep {
                                web_sys::console::log_1(
                                    &format!("Cleaning up stale PUBREC for packet {}", packet_id)
                                        .into(),
                                );
                            }
                            should_keep
                        });

                        state_ref.received_qos2.retain(|packet_id, timestamp| {
                            let should_keep = now - *timestamp <= cleanup_ms;
                            if !should_keep {
                                web_sys::console::log_1(
                                    &format!(
                                        "Cleaning up QoS 2 duplicate tracker for packet {}",
                                        packet_id
                                    )
                                    .into(),
                                );
                            }
                            should_keep
                        });
                    }
                    Err(_) => {
                        continue;
                    }
                }
            }
        });
    }

    fn handle_incoming_packet(state: &Rc<RefCell<ClientState>>, packet: Packet) {
        match packet {
            Packet::ConnAck(connack) => {
                web_sys::console::log_1(
                    &format!("CONNACK received: {:?}", connack.reason_code).into(),
                );

                let callback = state.borrow().on_connect.clone();
                if let Some(callback) = callback {
                    let reason_code = JsValue::from_f64(connack.reason_code as u8 as f64);
                    let session_present = JsValue::from_bool(connack.session_present);

                    if let Err(e) = callback.call2(&JsValue::NULL, &reason_code, &session_present) {
                        web_sys::console::error_1(
                            &format!("onConnect callback error: {:?}", e).into(),
                        );
                    }
                }
            }
            Packet::Publish(publish) => {
                let topic = publish.topic_name.clone();
                let payload = publish.payload.clone();
                let qos = publish.qos;

                web_sys::console::log_1(
                    &format!(
                        "PUBLISH received: topic={}, qos={:?}, payload_size={} bytes",
                        topic,
                        qos,
                        payload.len()
                    )
                    .into(),
                );

                if qos == mqtt5_protocol::QoS::ExactlyOnce {
                    if let Some(packet_id) = publish.packet_id {
                        let is_duplicate = state.borrow().received_qos2.contains_key(&packet_id);
                        let actions =
                            mqtt5_protocol::qos2::handle_incoming_publish_qos2(packet_id, is_duplicate);

                        for action in actions {
                            match action {
                                mqtt5_protocol::qos2::QoS2Action::DeliverMessage { packet_id: _ } => {
                                    let subscriptions = state.borrow().subscriptions.clone();
                                    let mut found_match = false;

                                    for (filter, callback) in subscriptions.iter() {
                                        if mqtt5_protocol::validation::topic_matches_filter(&topic, filter) {
                                            found_match = true;
                                            web_sys::console::log_1(
                                                &format!(
                                                    "Topic {} matches filter {}, calling callback",
                                                    topic, filter
                                                )
                                                .into(),
                                            );
                                            let topic_js = JsValue::from_str(&topic);
                                            let payload_array =
                                                js_sys::Uint8Array::from(&payload[..]);

                                            if let Err(e) = callback.call2(
                                                &JsValue::NULL,
                                                &topic_js,
                                                &payload_array.into(),
                                            ) {
                                                web_sys::console::error_1(
                                                    &format!("Callback error: {:?}", e).into(),
                                                );
                                            }
                                        }
                                    }

                                    if !found_match {
                                        web_sys::console::log_1(
                                            &format!(
                                                "No subscription filter matched topic: {}",
                                                topic
                                            )
                                            .into(),
                                        );
                                    }
                                }
                                mqtt5_protocol::qos2::QoS2Action::SendPubRec {
                                    packet_id,
                                    reason_code,
                                } => {
                                    let pubrec =
                                        mqtt5_protocol::packet::pubrec::PubRecPacket::new_with_reason(
                                            packet_id,
                                            reason_code,
                                        );
                                    let mut buf = BytesMut::new();
                                    if let Err(e) = encode_packet(
                                        &mqtt5_protocol::packet::Packet::PubRec(pubrec),
                                        &mut buf,
                                    ) {
                                        web_sys::console::error_1(
                                            &format!("PUBREC encode error: {}", e).into(),
                                        );
                                        continue;
                                    }

                                    let writer_rc = state.borrow().writer.clone();
                                    if let Some(writer_rc) = writer_rc {
                                        spawn_local(async move {
                                            if let Err(e) = writer_rc.borrow_mut().write(&buf).await
                                            {
                                                web_sys::console::error_1(
                                                    &format!("PUBREC send error: {}", e).into(),
                                                );
                                            }
                                        });
                                    }
                                }
                                mqtt5_protocol::qos2::QoS2Action::TrackIncomingPubRec { packet_id } => {
                                    let now = js_sys::Date::now();
                                    state.borrow_mut().pending_pubrecs.insert(packet_id, now);
                                    state.borrow_mut().received_qos2.insert(packet_id, now);
                                }
                                _ => {}
                            }
                        }
                    } else {
                        web_sys::console::error_1(&"QoS 2 PUBLISH missing packet_id".into());
                    }
                } else {
                    let subscriptions = state.borrow().subscriptions.clone();
                    let mut found_match = false;

                    for (filter, callback) in subscriptions.iter() {
                        if mqtt5_protocol::validation::topic_matches_filter(&topic, filter) {
                            found_match = true;
                            web_sys::console::log_1(
                                &format!(
                                    "Topic {} matches filter {}, calling callback",
                                    topic, filter
                                )
                                .into(),
                            );
                            let topic_js = JsValue::from_str(&topic);
                            let payload_array = js_sys::Uint8Array::from(&payload[..]);

                            if let Err(e) =
                                callback.call2(&JsValue::NULL, &topic_js, &payload_array.into())
                            {
                                web_sys::console::error_1(
                                    &format!("Callback error: {:?}", e).into(),
                                );
                            }
                        }
                    }

                    if !found_match {
                        web_sys::console::log_1(
                            &format!("No subscription filter matched topic: {}", topic).into(),
                        );
                    }
                }
            }
            Packet::SubAck(suback) => {
                web_sys::console::log_1(
                    &format!(
                        "SUBACK received: packet_id={}, reason_codes={:?}",
                        suback.packet_id, suback.reason_codes
                    )
                    .into(),
                );

                let callback = state.borrow_mut().pending_subacks.remove(&suback.packet_id);
                if let Some(callback) = callback {
                    web_sys::console::log_1(&"Calling SUBACK callback".into());
                    let reason_codes = suback
                        .reason_codes
                        .iter()
                        .map(|rc| JsValue::from_f64(*rc as u8 as f64))
                        .collect::<js_sys::Array>();

                    if let Err(e) = callback.call1(&JsValue::NULL, &reason_codes.into()) {
                        web_sys::console::error_1(
                            &format!("SUBACK callback error: {:?}", e).into(),
                        );
                    }
                } else {
                    web_sys::console::log_1(&"No pending SUBACK callback found".into());
                }
            }
            Packet::UnsubAck(unsuback) => {
                web_sys::console::log_1(
                    &format!(
                        "UNSUBACK received: packet_id={}, reason_codes={:?}",
                        unsuback.packet_id, unsuback.reason_codes
                    )
                    .into(),
                );
            }
            Packet::PingResp => {
                state.borrow_mut().last_pong_received = Some(js_sys::Date::now());
                web_sys::console::log_1(&"PINGRESP received".into());
            }
            Packet::PubAck(puback) => {
                let callback = state.borrow_mut().pending_pubacks.remove(&puback.packet_id);
                if let Some(callback) = callback {
                    let reason_code = JsValue::from_f64(puback.reason_code as u8 as f64);
                    if let Err(e) = callback.call1(&JsValue::NULL, &reason_code) {
                        web_sys::console::error_1(
                            &format!("PUBACK callback error: {:?}", e).into(),
                        );
                    }
                } else {
                    web_sys::console::log_1(
                        &format!("PUBACK received for packet {}", puback.packet_id).into(),
                    );
                }
            }
            Packet::PubRec(pubrec) => {
                web_sys::console::log_1(
                    &format!(
                        "PUBREC received: packet_id={}, reason_code={:?}",
                        pubrec.packet_id, pubrec.reason_code
                    )
                    .into(),
                );

                let has_pending = state
                    .borrow()
                    .pending_pubcomps
                    .contains_key(&pubrec.packet_id);
                let actions = mqtt5_protocol::qos2::handle_incoming_pubrec(
                    pubrec.packet_id,
                    pubrec.reason_code,
                    has_pending,
                );

                for action in actions {
                    match action {
                        mqtt5_protocol::qos2::QoS2Action::SendPubRel { packet_id } => {
                            let pubrel = mqtt5_protocol::packet::pubrel::PubRelPacket::new(packet_id);
                            let mut buf = BytesMut::new();
                            if let Err(e) =
                                encode_packet(&mqtt5_protocol::packet::Packet::PubRel(pubrel), &mut buf)
                            {
                                web_sys::console::error_1(
                                    &format!("PUBREL encode error: {}", e).into(),
                                );
                                continue;
                            }

                            let writer_rc = state.borrow().writer.clone();
                            if let Some(writer_rc) = writer_rc {
                                spawn_local(async move {
                                    if let Err(e) = writer_rc.borrow_mut().write(&buf).await {
                                        web_sys::console::error_1(
                                            &format!("PUBREL send error: {}", e).into(),
                                        );
                                    }
                                });
                            }
                        }
                        mqtt5_protocol::qos2::QoS2Action::ErrorFlow {
                            packet_id,
                            reason_code,
                        } => {
                            if let Some((callback, _)) =
                                state.borrow_mut().pending_pubcomps.remove(&packet_id)
                            {
                                let reason_code_js = JsValue::from_f64(reason_code as u8 as f64);
                                if let Err(e) = callback.call1(&JsValue::NULL, &reason_code_js) {
                                    web_sys::console::error_1(
                                        &format!("QoS 2 error callback error: {:?}", e).into(),
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Packet::PubComp(pubcomp) => {
                web_sys::console::log_1(
                    &format!(
                        "PUBCOMP received: packet_id={}, reason_code={:?}",
                        pubcomp.packet_id, pubcomp.reason_code
                    )
                    .into(),
                );

                let has_pending = state
                    .borrow()
                    .pending_pubcomps
                    .contains_key(&pubcomp.packet_id);
                let actions = mqtt5_protocol::qos2::handle_incoming_pubcomp(
                    pubcomp.packet_id,
                    pubcomp.reason_code,
                    has_pending,
                );

                for action in actions {
                    match action {
                        mqtt5_protocol::qos2::QoS2Action::CompleteFlow { packet_id } => {
                            if let Some((callback, _)) =
                                state.borrow_mut().pending_pubcomps.remove(&packet_id)
                            {
                                let reason_code_js =
                                    JsValue::from_f64(pubcomp.reason_code as u8 as f64);
                                if let Err(e) = callback.call1(&JsValue::NULL, &reason_code_js) {
                                    web_sys::console::error_1(
                                        &format!("PUBCOMP callback error: {:?}", e).into(),
                                    );
                                }
                            }
                        }
                        mqtt5_protocol::qos2::QoS2Action::ErrorFlow {
                            packet_id,
                            reason_code,
                        } => {
                            if let Some((callback, _)) =
                                state.borrow_mut().pending_pubcomps.remove(&packet_id)
                            {
                                let reason_code_js = JsValue::from_f64(reason_code as u8 as f64);
                                if let Err(e) = callback.call1(&JsValue::NULL, &reason_code_js) {
                                    web_sys::console::error_1(
                                        &format!("QoS 2 error callback error: {:?}", e).into(),
                                    );
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Packet::PubRel(pubrel) => {
                web_sys::console::log_1(
                    &format!("PUBREL received: packet_id={}", pubrel.packet_id).into(),
                );

                let has_pubrec = state
                    .borrow()
                    .pending_pubrecs
                    .contains_key(&pubrel.packet_id);
                let actions = mqtt5_protocol::qos2::handle_incoming_pubrel(pubrel.packet_id, has_pubrec);

                for action in actions {
                    match action {
                        mqtt5_protocol::qos2::QoS2Action::RemoveIncomingPubRec { packet_id } => {
                            state.borrow_mut().pending_pubrecs.remove(&packet_id);
                        }
                        mqtt5_protocol::qos2::QoS2Action::SendPubComp {
                            packet_id,
                            reason_code,
                        } => {
                            let pubcomp = mqtt5_protocol::packet::pubcomp::PubCompPacket::new_with_reason(
                                packet_id,
                                reason_code,
                            );
                            let mut buf = BytesMut::new();
                            if let Err(e) =
                                encode_packet(&mqtt5_protocol::packet::Packet::PubComp(pubcomp), &mut buf)
                            {
                                web_sys::console::error_1(
                                    &format!("PUBCOMP encode error: {}", e).into(),
                                );
                                continue;
                            }

                            let writer_rc = state.borrow().writer.clone();
                            if let Some(writer_rc) = writer_rc {
                                spawn_local(async move {
                                    if let Err(e) = writer_rc.borrow_mut().write(&buf).await {
                                        web_sys::console::error_1(
                                            &format!("PUBCOMP send error: {}", e).into(),
                                        );
                                    }
                                });
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                web_sys::console::warn_1(&format!("Unhandled packet type: {:?}", packet).into());
            }
        }
    }
}

#[wasm_bindgen]
impl WasmMqttClient {
    #[wasm_bindgen(constructor)]
    pub fn new(client_id: String) -> Self {
        console_error_panic_hook::set_once();

        Self {
            state: Rc::new(RefCell::new(ClientState::new(client_id))),
        }
    }

    pub async fn connect(&self, url: &str) -> Result<(), JsValue> {
        let transport = WasmTransportType::WebSocket(
            crate::transport::websocket::WasmWebSocketTransport::new(url),
        );
        self.connect_with_transport(transport).await
    }

    pub async fn connect_message_port(&self, port: MessagePort) -> Result<(), JsValue> {
        let transport = WasmTransportType::MessagePort(
            crate::transport::message_port::MessagePortTransport::new(port),
        );
        self.connect_with_transport(transport).await
    }

    pub async fn connect_broadcast_channel(&self, channel_name: &str) -> Result<(), JsValue> {
        let transport = WasmTransportType::BroadcastChannel(
            crate::transport::broadcast::BroadcastChannelTransport::new(channel_name),
        );
        self.connect_with_transport(transport).await
    }

    async fn connect_with_transport(
        &self,
        mut transport: WasmTransportType,
    ) -> Result<(), JsValue> {
        web_sys::console::log_1(&"Transport connecting...".into());
        transport
            .connect()
            .await
            .map_err(|e| JsValue::from_str(&format!("Transport connection failed: {}", e)))?;

        web_sys::console::log_1(&"Transport connected, sending CONNECT packet...".into());

        let client_id = self.state.borrow().client_id.clone();
        let connect_packet = ConnectPacket {
            protocol_version: 5,
            clean_start: true,
            keep_alive: 60,
            client_id,
            username: None,
            password: None,
            will: None,
            properties: Properties::default(),
            will_properties: Properties::default(),
        };

        let packet = Packet::Connect(Box::new(connect_packet));
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {}", e)))?;

        web_sys::console::log_1(&format!("Sending {} bytes...", buf.len()).into());

        transport
            .write(&buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;

        web_sys::console::log_1(&"CONNECT sent, splitting transport...".into());

        let (mut reader, writer) = transport
            .into_split()
            .map_err(|e| JsValue::from_str(&format!("Transport split failed: {}", e)))?;

        web_sys::console::log_1(&"Transport split, waiting for CONNACK...".into());

        let connack = read_packet(&mut reader)
            .await
            .map_err(|e| JsValue::from_str(&format!("CONNACK read failed: {}", e)))?;

        web_sys::console::log_1(&format!("Received packet: {:?}", connack).into());

        if let Packet::ConnAck(connack) = connack {
            let reason_code = connack.reason_code;
            let session_present = connack.session_present;

            self.state.borrow_mut().writer = Some(Rc::new(RefCell::new(writer)));
            self.state.borrow_mut().connected = true;

            self.spawn_packet_reader(reader);
            self.spawn_keepalive_task();
            self.spawn_qos2_cleanup_task();

            let callback = self.state.borrow().on_connect.clone();
            if let Some(callback) = callback {
                let reason_code_js = JsValue::from_f64(reason_code as u8 as f64);
                let session_present_js = JsValue::from_bool(session_present);

                if let Err(e) = callback.call2(&JsValue::NULL, &reason_code_js, &session_present_js)
                {
                    web_sys::console::error_1(&format!("onConnect callback error: {:?}", e).into());
                }
            }

            Ok(())
        } else {
            Err(JsValue::from_str(
                "Expected CONNACK, received different packet",
            ))
        }
    }

    pub async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), JsValue> {
        web_sys::console::log_1(
            &format!(
                "publish called for topic: {}, payload size: {} bytes",
                topic,
                payload.len()
            )
            .into(),
        );

        web_sys::console::log_1(&"Checking connection status...".into());
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        web_sys::console::log_1(&"Not connected, returning error".into());
                        return Err(JsValue::from_str("Not connected"));
                    }
                    web_sys::console::log_1(&"Connected, proceeding...".into());
                    break;
                }
                Err(_) => {
                    web_sys::console::log_1(&"State borrowed, retrying in 10ms...".into());
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        web_sys::console::log_1(&"Creating PUBLISH packet (QoS 0)...".into());
        let publish_packet = PublishPacket {
            dup: false,
            qos: QoS::AtMostOnce,
            retain: false,
            topic_name: topic.to_string(),
            packet_id: None,
            properties: Properties::default(),
            payload: payload.to_vec(),
        };

        let packet = Packet::Publish(publish_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {}", e)))?;

        web_sys::console::log_1(&format!("Sending PUBLISH packet ({} bytes)...", buf.len()).into());

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;

        web_sys::console::log_1(&"PUBLISH packet sent".into());
        web_sys::console::log_1(&"publish completed".into());
        Ok(())
    }

    pub async fn publish_qos1(
        &self,
        topic: &str,
        payload: &[u8],
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => break state.next_packet_id(),
                Err(_) => {
                    sleep_ms(10).await;
                    continue;
                }
            }
        };

        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    state.pending_pubacks.insert(packet_id, callback);
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        let publish_packet = PublishPacket {
            dup: false,
            qos: QoS::AtLeastOnce,
            retain: false,
            topic_name: topic.to_string(),
            packet_id: Some(packet_id),
            properties: Properties::default(),
            payload: payload.to_vec(),
        };

        let packet = Packet::Publish(publish_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {}", e)))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;

        Ok(packet_id)
    }

    pub async fn publish_qos2(
        &self,
        topic: &str,
        payload: &[u8],
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => break state.next_packet_id(),
                Err(_) => {
                    sleep_ms(10).await;
                    continue;
                }
            }
        };

        let now = js_sys::Date::now();
        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    state.pending_pubcomps.insert(packet_id, (callback, now));
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        let publish_packet = PublishPacket {
            dup: false,
            qos: QoS::ExactlyOnce,
            retain: false,
            topic_name: topic.to_string(),
            packet_id: Some(packet_id),
            properties: Properties::default(),
            payload: payload.to_vec(),
        };

        let packet = Packet::Publish(publish_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {}", e)))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;

        Ok(packet_id)
    }

    pub async fn subscribe(&self, topic: &str) -> Result<u16, JsValue> {
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        return Err(JsValue::from_str("Not connected"));
                    }
                    break;
                }
                Err(_) => {
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => break state.next_packet_id(),
                Err(_) => {
                    sleep_ms(10).await;
                    continue;
                }
            }
        };

        let subscribe_packet = SubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![mqtt5_protocol::packet::subscribe::TopicFilter::new(
                topic,
                QoS::AtMostOnce,
            )],
        };

        let packet = Packet::Subscribe(subscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {}", e)))?;

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;

        Ok(packet_id)
    }

    pub async fn subscribe_with_callback(
        &self,
        topic: &str,
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        web_sys::console::log_1(
            &format!("subscribe_with_callback called for topic: {}", topic).into(),
        );

        web_sys::console::log_1(&"Checking connection status...".into());
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        web_sys::console::log_1(&"Not connected, returning error".into());
                        return Err(JsValue::from_str("Not connected"));
                    }
                    web_sys::console::log_1(&"Connected, proceeding...".into());
                    break;
                }
                Err(_) => {
                    web_sys::console::log_1(&"State borrowed, retrying in 10ms...".into());
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        web_sys::console::log_1(&"Getting packet ID...".into());
        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    let id = state.next_packet_id();
                    web_sys::console::log_1(&format!("Got packet ID: {}", id).into());
                    break id;
                }
                Err(_) => {
                    web_sys::console::log_1(&"State mutably borrowed, retrying in 10ms...".into());
                    sleep_ms(10).await;
                    continue;
                }
            }
        };

        web_sys::console::log_1(&"Storing subscription callback...".into());
        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    state.subscriptions.insert(topic.to_string(), callback);
                    web_sys::console::log_1(&"Callback stored".into());
                    break;
                }
                Err(_) => {
                    web_sys::console::log_1(&"State mutably borrowed, retrying in 10ms...".into());
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        web_sys::console::log_1(&"Creating SUBSCRIBE packet...".into());
        let subscribe_packet = SubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![mqtt5_protocol::packet::subscribe::TopicFilter::new(
                topic,
                QoS::AtMostOnce,
            )],
        };

        let packet = Packet::Subscribe(subscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {}", e)))?;

        web_sys::console::log_1(
            &format!("Sending SUBSCRIBE packet ({} bytes)...", buf.len()).into(),
        );

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;

        web_sys::console::log_1(&"SUBSCRIBE packet sent".into());
        web_sys::console::log_1(
            &format!(
                "subscribe_with_callback completed, packet_id: {}",
                packet_id
            )
            .into(),
        );
        Ok(packet_id)
    }

    pub async fn unsubscribe(&self, topic: &str) -> Result<u16, JsValue> {
        web_sys::console::log_1(&format!("unsubscribe called for topic: {}", topic).into());

        web_sys::console::log_1(&"Checking connection status...".into());
        loop {
            match self.state.try_borrow() {
                Ok(state) => {
                    if !state.connected {
                        web_sys::console::log_1(&"Not connected, returning error".into());
                        return Err(JsValue::from_str("Not connected"));
                    }
                    web_sys::console::log_1(&"Connected, proceeding...".into());
                    break;
                }
                Err(_) => {
                    web_sys::console::log_1(&"State borrowed, retrying in 10ms...".into());
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        web_sys::console::log_1(&"Getting packet ID...".into());
        let packet_id = loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    let id = state.next_packet_id();
                    web_sys::console::log_1(&format!("Got packet ID: {}", id).into());
                    break id;
                }
                Err(_) => {
                    web_sys::console::log_1(&"State mutably borrowed, retrying in 10ms...".into());
                    sleep_ms(10).await;
                    continue;
                }
            }
        };

        web_sys::console::log_1(&"Removing subscription from state...".into());
        loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    state.subscriptions.remove(topic);
                    web_sys::console::log_1(&"Subscription removed".into());
                    break;
                }
                Err(_) => {
                    web_sys::console::log_1(&"State mutably borrowed, retrying in 10ms...".into());
                    sleep_ms(10).await;
                    continue;
                }
            }
        }

        web_sys::console::log_1(&"Creating UNSUBSCRIBE packet...".into());
        let unsubscribe_packet = UnsubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![topic.to_string()],
        };

        let packet = Packet::Unsubscribe(unsubscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {}", e)))?;

        web_sys::console::log_1(
            &format!("Sending UNSUBSCRIBE packet ({} bytes)...", buf.len()).into(),
        );

        let writer_rc = self
            .state
            .borrow()
            .writer
            .clone()
            .ok_or_else(|| JsValue::from_str("Writer disconnected"))?;

        writer_rc
            .borrow_mut()
            .write(&buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;

        web_sys::console::log_1(&"UNSUBSCRIBE packet sent".into());
        web_sys::console::log_1(&format!("unsubscribe completed, packet_id: {}", packet_id).into());
        Ok(packet_id)
    }

    pub async fn disconnect(&self) -> Result<(), JsValue> {
        web_sys::console::log_1(&"disconnect called".into());

        web_sys::console::log_1(&"Sending DISCONNECT packet...".into());
        let disconnect_packet = mqtt5_protocol::packet::disconnect::DisconnectPacket {
            reason_code: mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::Success,
            properties: Properties::default(),
        };
        let packet = Packet::Disconnect(disconnect_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("DISCONNECT packet encoding failed: {}", e)))?;

        let writer_rc = self.state.borrow().writer.clone();
        if let Some(writer_rc) = writer_rc {
            writer_rc
                .borrow_mut()
                .write(&buf)
                .await
                .map_err(|e| JsValue::from_str(&format!("DISCONNECT packet send failed: {}", e)))?;
            web_sys::console::log_1(&"DISCONNECT packet sent".into());
        }

        web_sys::console::log_1(&"Marking disconnected and taking writer...".into());
        let writer_rc = loop {
            match self.state.try_borrow_mut() {
                Ok(mut state) => {
                    state.connected = false;
                    let w = state.writer.take();
                    web_sys::console::log_1(
                        &format!("Writer taken, is_some: {}", w.is_some()).into(),
                    );
                    break w;
                }
                Err(_) => {
                    web_sys::console::log_1(&"State mutably borrowed, retrying in 10ms...".into());
                    sleep_ms(10).await;
                    continue;
                }
            }
        };

        if let Some(writer_rc) = writer_rc {
            web_sys::console::log_1(&"Closing writer...".into());
            writer_rc
                .borrow_mut()
                .close()
                .await
                .map_err(|e| JsValue::from_str(&format!("Close failed: {}", e)))?;
            web_sys::console::log_1(&"Writer closed".into());
        }

        web_sys::console::log_1(&"Triggering disconnect callback...".into());
        Self::trigger_disconnect_callback(&self.state);
        web_sys::console::log_1(&"disconnect completed".into());
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.state.borrow().connected
    }

    pub fn on_connect(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_connect = Some(callback);
    }

    pub fn on_disconnect(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_disconnect = Some(callback);
    }

    pub fn on_error(&self, callback: js_sys::Function) {
        self.state.borrow_mut().on_error = Some(callback);
    }
}
