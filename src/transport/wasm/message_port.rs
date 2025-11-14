use crate::error::{MqttError, Result};
use crate::transport::Transport;
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

impl MessagePortTransport {
    pub fn new(port: MessagePort) -> Self {
        Self {
            port,
            rx: None,
            connected: Arc::new(AtomicBool::new(false)),
            _closure: None,
        }
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
