use crate::packet::connect::ConnectPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::subscribe::SubscribePacket;
use crate::packet::{MqttPacket, Packet};
use crate::protocol::v5::properties::Properties;
use crate::transport::{Transport, WasmTransportType};
use crate::QoS;
use bytes::BytesMut;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
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
        Packet::PingReq => {
            crate::packet::pingreq::PingReqPacket::default().encode(buf)
        }
        Packet::Disconnect(p) => p.encode(buf),
        Packet::Unsubscribe(p) => p.encode(buf),
        _ => Err(crate::error::MqttError::ProtocolError(
            format!("Encoding not yet implemented for packet type: {:?}", packet),
        )),
    }
}

#[wasm_bindgen]
pub struct WasmMqttClient {
    state: Rc<RefCell<ClientState>>,
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
