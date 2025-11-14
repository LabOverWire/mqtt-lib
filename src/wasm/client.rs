use crate::packet::connect::ConnectPacket;
use crate::packet::publish::PublishPacket;
use crate::packet::subscribe::SubscribePacket;
use crate::packet::{MqttPacket, Packet, PacketType};
use crate::protocol::v5::properties::Properties;
use crate::transport::{Transport, WasmTransportType};
use crate::QoS;
use bytes::{BufMut, BytesMut};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;

struct ClientState {
    client_id: String,
    transport: Option<WasmTransportType>,
    packet_id: u16,
    connected: bool,
}

impl ClientState {
    fn new(client_id: String) -> Self {
        Self {
            client_id,
            transport: None,
            packet_id: 0,
            connected: false,
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

fn encode_packet_simple(packet: &Packet, buf: &mut BytesMut) -> crate::error::Result<()> {
    match packet {
        Packet::Connect(p) => {
            let flags = 0;
            let packet_type_byte = (u8::from(PacketType::Connect) << 4) | flags;
            buf.put_u8(packet_type_byte);

            let mut body = BytesMut::new();
            p.encode_body(&mut body)?;

            crate::encoding::variable_int::encode_variable_int(
                buf,
                body.len().try_into().unwrap_or(u32::MAX),
            )?;
            buf.put(body);
        }
        Packet::Publish(p) => {
            let flags = p.flags();
            let packet_type_byte = (u8::from(PacketType::Publish) << 4) | flags;
            buf.put_u8(packet_type_byte);

            let mut body = BytesMut::new();
            p.encode_body(&mut body)?;

            crate::encoding::variable_int::encode_variable_int(
                buf,
                body.len().try_into().unwrap_or(u32::MAX),
            )?;
            buf.put(body);
        }
        Packet::Subscribe(p) => {
            let flags = 0x02;
            let packet_type_byte = (u8::from(PacketType::Subscribe) << 4) | flags;
            buf.put_u8(packet_type_byte);

            let mut body = BytesMut::new();
            p.encode_body(&mut body)?;

            crate::encoding::variable_int::encode_variable_int(
                buf,
                body.len().try_into().unwrap_or(u32::MAX),
            )?;
            buf.put(body);
        }
        _ => return Err(crate::error::MqttError::ProtocolError("Unsupported packet type for WASM client".to_string())),
    }
    Ok(())
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
        let mut transport = WasmTransportType::WebSocket(crate::transport::wasm::websocket::WasmWebSocketTransport::new(url));
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
        encode_packet_simple(&packet, &mut buf)
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
        encode_packet_simple(&packet, &mut buf)
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
        encode_packet_simple(&packet, &mut buf)
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
