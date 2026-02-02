use futures::channel::{mpsc, oneshot};
use futures::StreamExt;
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::Transport;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CloseEvent, ErrorEvent, MessageEvent, WebSocket};

pub struct WasmWebSocketTransport {
    url: String,
    ws: Option<WebSocket>,
    rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    connected: Arc<AtomicBool>,
    closures: Option<ClosureBundle>,
    buffer: Vec<u8>,
}

struct ClosureBundle {
    _onmessage: Closure<dyn FnMut(MessageEvent)>,
    _onopen: Closure<dyn FnMut(JsValue)>,
    _onerror: Closure<dyn FnMut(ErrorEvent)>,
    _onclose: Closure<dyn FnMut(CloseEvent)>,
}

pub struct WasmReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    buffer: Vec<u8>,
    connected: Arc<AtomicBool>,
}

pub struct WasmWriter {
    ws: WebSocket,
    connected: Arc<AtomicBool>,
    _closures: ClosureBundle,
}

impl WasmReader {
    /// # Errors
    /// Returns an error if the connection is closed or if there is no data available.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !self.buffer.is_empty() {
            let len = self.buffer.len().min(buf.len());
            buf[..len].copy_from_slice(&self.buffer[..len]);
            self.buffer.drain(..len);
            return Ok(len);
        }

        let data = self
            .rx
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

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl WasmWriter {
    /// # Errors
    /// Returns an error if the WebSocket send operation fails.
    pub fn write(&mut self, buf: &[u8]) -> Result<()> {
        self.ws
            .send_with_u8_array(buf)
            .map_err(|e| MqttError::Io(format!("WebSocket send failed: {e:?}")))?;
        Ok(())
    }

    /// # Errors
    /// This method does not currently return errors but uses Result for API consistency.
    pub fn close(&mut self) -> Result<()> {
        self.ws.set_onmessage(None);
        self.ws.set_onopen(None);
        self.ws.set_onerror(None);
        self.ws.set_onclose(None);
        self.ws.close().ok();
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl Drop for WasmWriter {
    fn drop(&mut self) {
        self.ws.set_onmessage(None);
        self.ws.set_onopen(None);
        self.ws.set_onerror(None);
        self.ws.set_onclose(None);
        self.ws.close().ok();
    }
}

impl WasmWebSocketTransport {
    #[allow(clippy::must_use_candidate)]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ws: None,
            rx: None,
            connected: Arc::new(AtomicBool::new(false)),
            closures: None,
            buffer: Vec::new(),
        }
    }

    /// # Errors
    /// Returns an error if the transport is not connected.
    pub fn into_split(self) -> Result<(WasmReader, WasmWriter)> {
        let ws = self.ws.ok_or(MqttError::NotConnected)?;
        let rx = self.rx.ok_or(MqttError::NotConnected)?;
        let closures = self.closures.ok_or(MqttError::NotConnected)?;

        let reader = WasmReader {
            rx,
            buffer: self.buffer,
            connected: Arc::clone(&self.connected),
        };

        let writer = WasmWriter {
            ws,
            connected: self.connected,
            _closures: closures,
        };

        Ok((reader, writer))
    }
}

impl Transport for WasmWebSocketTransport {
    async fn connect(&mut self) -> Result<()> {
        let ws = WebSocket::new_with_str(&self.url, "mqtt").map_err(|e| {
            MqttError::ConnectionError(format!("Failed to create WebSocket: {e:?}"))
        })?;

        ws.set_binary_type(web_sys::BinaryType::Arraybuffer);

        let (msg_tx, msg_rx) = mpsc::unbounded();
        let (result_tx, result_rx) = oneshot::channel();

        let msg_tx_clone = msg_tx.clone();
        let onmessage = Closure::new(move |e: MessageEvent| {
            if let Ok(abuf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                let array = js_sys::Uint8Array::new(&abuf);
                let vec = array.to_vec();
                let _ = msg_tx_clone.unbounded_send(vec);
            } else {
                web_sys::console::warn_1(&"WebSocket received non-ArrayBuffer message".into());
            }
        });

        let result_tx = Rc::new(Cell::new(Some(result_tx)));

        let result_tx_open = result_tx.clone();
        let connected_clone = self.connected.clone();
        let onopen = Closure::new(move |_: JsValue| {
            connected_clone.store(true, Ordering::SeqCst);
            if let Some(tx) = result_tx_open.take() {
                let _ = tx.send(Ok(()));
            }
        });

        let result_tx_error = result_tx;
        let connected_clone2 = self.connected.clone();
        let onerror = Closure::new(move |_e: ErrorEvent| {
            connected_clone2.store(false, Ordering::SeqCst);
            if let Some(tx) = result_tx_error.take() {
                let _ = tx.send(Err(MqttError::ConnectionError(
                    "WebSocket connection failed".into(),
                )));
            }
        });

        let connected_clone3 = self.connected.clone();
        let onclose = Closure::new(move |_e: CloseEvent| {
            connected_clone3.store(false, Ordering::SeqCst);
        });

        ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        ws.set_onopen(Some(onopen.as_ref().unchecked_ref()));
        ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));

        self.ws = Some(ws.clone());
        self.rx = Some(msg_rx);
        self.closures = Some(ClosureBundle {
            _onmessage: onmessage,
            _onopen: onopen,
            _onerror: onerror,
            _onclose: onclose,
        });

        let result = result_rx
            .await
            .map_err(|_| MqttError::ConnectionError("Connection cancelled".into()))?;

        if result.is_err() {
            ws.set_onmessage(None);
            ws.set_onopen(None);
            ws.set_onerror(None);
            ws.set_onclose(None);
            ws.close().ok();
            self.ws = None;
            self.rx = None;
            self.closures = None;
        }

        result
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
        let ws = self.ws.as_ref().ok_or(MqttError::NotConnected)?;

        ws.send_with_u8_array(buf)
            .map_err(|e| MqttError::Io(format!("WebSocket send failed: {e:?}")))?;

        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(ws) = self.ws.take() {
            ws.close().ok();
        }
        self.connected.store(false, Ordering::SeqCst);
        self.closures = None;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}
