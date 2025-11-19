use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::Transport;
use futures::channel::mpsc;
use futures::StreamExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, MessagePort};

pub struct MessagePortTransport {
    port: MessagePort,
    rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    connected: Arc<AtomicBool>,
    _closure: Option<Closure<dyn FnMut(MessageEvent)>>,
}

pub struct MessagePortReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    connected: Arc<AtomicBool>,
    buffer: Vec<u8>,
    buffer_pos: usize,
}

pub struct MessagePortWriter {
    port: MessagePort,
    connected: Arc<AtomicBool>,
    _closure: Closure<dyn FnMut(MessageEvent)>,
}

impl MessagePortReader {
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.buffer_pos < self.buffer.len() {
            let available = self.buffer.len() - self.buffer_pos;
            let to_copy = available.min(buf.len());
            buf[..to_copy]
                .copy_from_slice(&self.buffer[self.buffer_pos..self.buffer_pos + to_copy]);
            self.buffer_pos += to_copy;

            if self.buffer_pos >= self.buffer.len() {
                self.buffer.clear();
                self.buffer_pos = 0;
            }

            return Ok(to_copy);
        }

        let data = self
            .rx
            .next()
            .await
            .ok_or(MqttError::ConnectionClosedByPeer)?;

        let to_copy = data.len().min(buf.len());
        buf[..to_copy].copy_from_slice(&data[..to_copy]);

        if to_copy < data.len() {
            self.buffer = data[to_copy..].to_vec();
            self.buffer_pos = 0;
        }

        Ok(to_copy)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl MessagePortWriter {
    pub async fn write(&mut self, buf: &[u8]) -> Result<()> {
        let array = js_sys::Uint8Array::from(buf);
        self.port
            .post_message(&array.buffer())
            .map_err(|e| MqttError::Io(format!("MessagePort send failed: {:?}", e)))?;
        Ok(())
    }

    pub async fn close(&mut self) -> Result<()> {
        self.port.close();
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl MessagePortTransport {
    pub fn new(port: MessagePort) -> Self {
        Self {
            port,
            rx: None,
            connected: Arc::new(AtomicBool::new(false)),
            _closure: None,
        }
    }

    pub fn into_split(self) -> Result<(MessagePortReader, MessagePortWriter)> {
        let port = self.port;
        let rx = self.rx.ok_or(MqttError::NotConnected)?;
        let closure = self._closure.ok_or(MqttError::NotConnected)?;

        let reader = MessagePortReader {
            rx,
            connected: Arc::clone(&self.connected),
            buffer: Vec::new(),
            buffer_pos: 0,
        };

        let writer = MessagePortWriter {
            port,
            connected: self.connected,
            _closure: closure,
        };

        Ok((reader, writer))
    }
}

impl Transport for MessagePortTransport {
    async fn connect(&mut self) -> Result<()> {
        let (msg_tx, msg_rx) = mpsc::unbounded();

        let msg_tx_clone = msg_tx.clone();
        let onmessage = Closure::new(move |e: MessageEvent| {
            if let Ok(abuf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                let array = js_sys::Uint8Array::new(&abuf);
                let vec = array.to_vec();
                let _ = msg_tx_clone.unbounded_send(vec);
            }
        });

        self.port
            .set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

        self.port.start();

        self.rx = Some(msg_rx);
        self._closure = Some(onmessage);
        self.connected.store(true, Ordering::SeqCst);

        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let data = self
            .rx
            .as_mut()
            .ok_or(MqttError::NotConnected)?
            .next()
            .await
            .ok_or(MqttError::ConnectionClosedByPeer)?;

        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        let array = js_sys::Uint8Array::from(buf);
        self.port
            .post_message(&array.buffer())
            .map_err(|e| MqttError::Io(format!("MessagePort send failed: {:?}", e)))?;

        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        self.port.close();
        self.connected.store(false, Ordering::SeqCst);
        self._closure = None;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}
