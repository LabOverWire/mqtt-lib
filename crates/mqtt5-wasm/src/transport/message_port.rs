use futures::channel::mpsc;
use futures::StreamExt;
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::Transport;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{MessageEvent, MessagePort};

pub struct MessagePortTransport {
    port: MessagePort,
    rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    connected: Arc<AtomicBool>,
    closure: Option<Closure<dyn FnMut(MessageEvent)>>,
    buffer: Vec<u8>,
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
    msg_tx: mpsc::UnboundedSender<Vec<u8>>,
    _closure: Option<Closure<dyn FnMut(MessageEvent)>>,
}

impl MessagePortReader {
    #[allow(clippy::must_use_candidate)]
    pub fn new(rx: mpsc::UnboundedReceiver<Vec<u8>>, connected: Arc<AtomicBool>) -> Self {
        Self {
            rx,
            connected,
            buffer: Vec::new(),
            buffer_pos: 0,
        }
    }

    /// # Errors
    /// Returns an error if reading from the port fails.
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

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl MessagePortWriter {
    #[allow(clippy::must_use_candidate)]
    pub fn new(
        port: MessagePort,
        connected: Arc<AtomicBool>,
        msg_tx: mpsc::UnboundedSender<Vec<u8>>,
    ) -> Self {
        Self {
            port,
            connected,
            msg_tx,
            _closure: None,
        }
    }

    /// # Errors
    /// Returns an error if writing to the port fails.
    pub fn write(&mut self, buf: &[u8]) -> Result<()> {
        let array = js_sys::Uint8Array::from(buf);
        self.port
            .post_message(&array.buffer())
            .map_err(|e| MqttError::Io(format!("MessagePort send failed: {e:?}")))?;
        Ok(())
    }

    /// # Errors
    /// Returns an error if closing the port fails.
    pub fn close(&mut self) -> Result<()> {
        self.msg_tx.close_channel();
        self.port.close();
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl Drop for MessagePortWriter {
    fn drop(&mut self) {
        self.msg_tx.close_channel();
        self.connected.store(false, Ordering::SeqCst);
        self.port.set_onmessage(None);
        self.port.close();
    }
}

impl MessagePortTransport {
    #[allow(clippy::must_use_candidate)]
    pub fn new(port: MessagePort) -> Self {
        Self {
            port,
            rx: None,
            tx: None,
            connected: Arc::new(AtomicBool::new(false)),
            closure: None,
            buffer: Vec::new(),
        }
    }

    /// # Errors
    /// Returns an error if the transport is not connected.
    pub fn into_split(self) -> Result<(MessagePortReader, MessagePortWriter)> {
        let port = self.port;
        let rx = self.rx.ok_or(MqttError::NotConnected)?;
        let closure = self.closure.ok_or(MqttError::NotConnected)?;
        let msg_tx = self.tx.ok_or(MqttError::NotConnected)?;

        let reader = MessagePortReader {
            rx,
            connected: Arc::clone(&self.connected),
            buffer: self.buffer,
            buffer_pos: 0,
        };

        let writer = MessagePortWriter {
            port,
            connected: self.connected,
            msg_tx,
            _closure: Some(closure),
        };

        Ok((reader, writer))
    }
}

impl Transport for MessagePortTransport {
    async fn connect(&mut self) -> Result<()> {
        let (msg_tx, msg_rx) = mpsc::unbounded();

        let msg_tx_clone = msg_tx.clone();
        let onmessage: Closure<dyn FnMut(MessageEvent)> =
            Closure::wrap(Box::new(move |e: MessageEvent| {
                if let Ok(abuf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                    let array = js_sys::Uint8Array::new(&abuf);
                    let vec = array.to_vec();
                    let _ = msg_tx_clone.unbounded_send(vec);
                }
            }));

        self.port
            .set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

        self.port.start();

        self.rx = Some(msg_rx);
        self.tx = Some(msg_tx);
        self.closure = Some(onmessage);
        self.connected.store(true, Ordering::SeqCst);

        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !self.buffer.is_empty() {
            let len = self.buffer.len().min(buf.len());
            buf[..len].copy_from_slice(&self.buffer[..len]);
            self.buffer.drain(..len);
            return Ok(len);
        }

        let data = self
            .rx
            .as_mut()
            .ok_or(MqttError::NotConnected)?
            .next()
            .await
            .ok_or(MqttError::ConnectionClosedByPeer)?;

        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);

        if data.len() > len {
            self.buffer.extend_from_slice(&data[len..]);
        }

        Ok(len)
    }

    async fn write(&mut self, buf: &[u8]) -> Result<()> {
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }

        let array = js_sys::Uint8Array::from(buf);
        self.port
            .post_message(&array.buffer())
            .map_err(|e| MqttError::Io(format!("MessagePort send failed: {e:?}")))?;

        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        self.port.close();
        self.connected.store(false, Ordering::SeqCst);
        self.closure = None;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}
