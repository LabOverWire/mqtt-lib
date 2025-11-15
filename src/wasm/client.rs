use crate::packet::connect::ConnectPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::subscribe::SubscribePacket;
use crate::packet::{MqttPacket, Packet};
use crate::protocol::v5::properties::Properties;
use crate::transport::{Transport, WasmTransportType};
use crate::wasm::decoder::read_packet;
use crate::QoS;
use bytes::BytesMut;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::MessagePort;

struct ClientState {
    client_id: String,
    transport: Option<WasmTransportType>,
    packet_id: u16,
    connected: bool,
    subscriptions: HashMap<String, js_sys::Function>,
    pending_subacks: HashMap<u16, js_sys::Function>,
    keep_alive: u16,
}

impl ClientState {
    fn new(client_id: String) -> Self {
        Self {
            client_id,
            transport: None,
            packet_id: 0,
            connected: false,
            subscriptions: HashMap::new(),
            pending_subacks: HashMap::new(),
            keep_alive: 60,
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

fn encode_packet(packet: &Packet, buf: &mut BytesMut) -> crate::error::Result<()> {
    match packet {
        Packet::Connect(p) => p.encode(buf),
        Packet::Publish(p) => p.encode(buf),
        Packet::Subscribe(p) => p.encode(buf),
        Packet::PingReq => crate::packet::pingreq::PingReqPacket::default().encode(buf),
        Packet::Disconnect(p) => p.encode(buf),
        Packet::Unsubscribe(p) => p.encode(buf),
        _ => Err(crate::error::MqttError::ProtocolError(format!(
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
    fn spawn_packet_reader(&self) {
        let state = Rc::clone(&self.state);

        spawn_local(async move {
            loop {
                let packet = {
                    let mut state_ref = state.borrow_mut();
                    if let Some(transport) = &mut state_ref.transport {
                        match read_packet(transport).await {
                            Ok(packet) => packet,
                            Err(e) => {
                                web_sys::console::error_1(&format!("Packet read error: {}", e).into());
                                state_ref.connected = false;
                                break;
                            }
                        }
                    } else {
                        break;
                    }
                };

                Self::handle_incoming_packet(&state, packet);
            }
        });
    }

    fn handle_incoming_packet(state: &Rc<RefCell<ClientState>>, packet: Packet) {
        match packet {
            Packet::ConnAck(connack) => {
                web_sys::console::log_1(&format!("CONNACK received: {:?}", connack.reason_code).into());
            }
            Packet::Publish(publish) => {
                let topic = publish.topic_name.clone();
                let payload = publish.payload.clone();

                let callback = state.borrow().subscriptions.get(&topic).cloned();
                if let Some(callback) = callback {
                    let topic_js = JsValue::from_str(&topic);
                    let payload_array = js_sys::Uint8Array::from(&payload[..]);

                    if let Err(e) = callback.call2(&JsValue::NULL, &topic_js, &payload_array.into()) {
                        web_sys::console::error_1(&format!("Callback error: {:?}", e).into());
                    }
                }
            }
            Packet::SubAck(suback) => {
                let callback = state.borrow_mut().pending_subacks.remove(&suback.packet_id);
                if let Some(callback) = callback {
                    let reason_codes = suback.reason_codes.iter()
                        .map(|rc| JsValue::from_f64(*rc as u8 as f64))
                        .collect::<js_sys::Array>();

                    if let Err(e) = callback.call1(&JsValue::NULL, &reason_codes.into()) {
                        web_sys::console::error_1(&format!("SUBACK callback error: {:?}", e).into());
                    }
                }
            }
            Packet::PingResp => {
                web_sys::console::log_1(&"PINGRESP received".into());
            }
            Packet::PubAck(puback) => {
                web_sys::console::log_1(&format!("PUBACK received for packet {}", puback.packet_id).into());
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
            crate::transport::wasm::websocket::WasmWebSocketTransport::new(url),
        );
        self.connect_with_transport(transport).await
    }

    pub async fn connect_message_port(&self, port: MessagePort) -> Result<(), JsValue> {
        let transport = WasmTransportType::MessagePort(
            crate::transport::wasm::message_port::MessagePortTransport::new(port),
        );
        self.connect_with_transport(transport).await
    }

    pub async fn connect_broadcast_channel(&self, channel_name: &str) -> Result<(), JsValue> {
        let transport = WasmTransportType::BroadcastChannel(
            crate::transport::wasm::broadcast::BroadcastChannelTransport::new(channel_name),
        );
        self.connect_with_transport(transport).await
    }

    async fn connect_with_transport(
        &self,
        mut transport: WasmTransportType,
    ) -> Result<(), JsValue> {
        transport
            .connect()
            .await
            .map_err(|e| JsValue::from_str(&format!("Transport connection failed: {}", e)))?;

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

        transport
            .write(&buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;

        let mut connack_buf = vec![0u8; 1024];
        let _n = transport
            .read(&mut connack_buf)
            .await
            .map_err(|e| JsValue::from_str(&format!("Read failed: {}", e)))?;

        self.state.borrow_mut().transport = Some(transport);
        self.state.borrow_mut().connected = true;

        self.spawn_packet_reader();

        Ok(())
    }

    pub async fn publish(&self, topic: &str, payload: &[u8]) -> Result<(), JsValue> {
        if !self.state.borrow().connected {
            return Err(JsValue::from_str("Not connected"));
        }

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

        let mut state = self.state.borrow_mut();
        if let Some(transport) = &mut state.transport {
            transport
                .write(&buf)
                .await
                .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;
        }

        Ok(())
    }

    pub async fn subscribe(&self, topic: &str) -> Result<u16, JsValue> {
        if !self.state.borrow().connected {
            return Err(JsValue::from_str("Not connected"));
        }

        let packet_id = self.state.borrow_mut().next_packet_id();

        let subscribe_packet = SubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![crate::packet::subscribe::TopicFilter::new(
                topic,
                QoS::AtMostOnce,
            )],
        };

        let packet = Packet::Subscribe(subscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {}", e)))?;

        let mut state = self.state.borrow_mut();
        if let Some(transport) = &mut state.transport {
            transport
                .write(&buf)
                .await
                .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;
        }

        Ok(packet_id)
    }

    pub async fn subscribe_with_callback(
        &self,
        topic: &str,
        callback: js_sys::Function,
    ) -> Result<u16, JsValue> {
        if !self.state.borrow().connected {
            return Err(JsValue::from_str("Not connected"));
        }

        let packet_id = self.state.borrow_mut().next_packet_id();

        self.state.borrow_mut().subscriptions.insert(topic.to_string(), callback);

        let subscribe_packet = SubscribePacket {
            packet_id,
            properties: Properties::default(),
            filters: vec![crate::packet::subscribe::TopicFilter::new(
                topic,
                QoS::AtMostOnce,
            )],
        };

        let packet = Packet::Subscribe(subscribe_packet);
        let mut buf = BytesMut::new();
        encode_packet(&packet, &mut buf)
            .map_err(|e| JsValue::from_str(&format!("Packet encoding failed: {}", e)))?;

        let mut state = self.state.borrow_mut();
        if let Some(transport) = &mut state.transport {
            transport
                .write(&buf)
                .await
                .map_err(|e| JsValue::from_str(&format!("Write failed: {}", e)))?;
        }

        Ok(packet_id)
    }

    pub async fn disconnect(&self) -> Result<(), JsValue> {
        let mut state = self.state.borrow_mut();
        if let Some(mut transport) = state.transport.take() {
            transport
                .close()
                .await
                .map_err(|e| JsValue::from_str(&format!("Close failed: {}", e)))?;
        }
        state.connected = false;
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.state.borrow().connected
    }
}
