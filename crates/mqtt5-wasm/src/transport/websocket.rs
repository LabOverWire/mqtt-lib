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
    tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    connected: Arc<AtomicBool>,
    closures: Option<ClosureBundle>,
    buffer: Vec<u8>,
}

struct ClosureBundle {
    onmessage: Closure<dyn FnMut(MessageEvent)>,
    onopen: Closure<dyn FnMut(JsValue)>,
    onerror: Closure<dyn FnMut(ErrorEvent)>,
    onclose: Closure<dyn FnMut(CloseEvent)>,
}

impl ClosureBundle {
    fn attach(&self, ws: &WebSocket) {
        ws.set_onmessage(Some(self.onmessage.as_ref().unchecked_ref()));
        ws.set_onopen(Some(self.onopen.as_ref().unchecked_ref()));
        ws.set_onerror(Some(self.onerror.as_ref().unchecked_ref()));
        ws.set_onclose(Some(self.onclose.as_ref().unchecked_ref()));
    }
}

pub struct WasmReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    buffer: Vec<u8>,
    connected: Arc<AtomicBool>,
}

pub struct WasmWriter {
    ws: WebSocket,
    connected: Arc<AtomicBool>,
    msg_tx: mpsc::UnboundedSender<Vec<u8>>,
    closures: Option<ClosureBundle>,
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
        if !self.is_connected() {
            return Err(MqttError::NotConnected);
        }
        self.ws
            .send_with_u8_array(buf)
            .map_err(|e| MqttError::Io(format!("WebSocket send failed: {e:?}")))?;
        Ok(())
    }

    /// # Errors
    /// This method does not currently return errors but uses Result for API consistency.
    pub fn close(&mut self) -> Result<()> {
        self.shutdown();
        Ok(())
    }

    fn shutdown(&mut self) {
        self.msg_tx.close_channel();
        self.connected.store(false, Ordering::SeqCst);
        if self.closures.take().is_some() {
            self.ws.set_onmessage(None);
            self.ws.set_onopen(None);
            self.ws.set_onerror(None);
            self.ws.set_onclose(None);
        }
        self.ws.close().ok();
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl Drop for WasmWriter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl WasmWebSocketTransport {
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ws: None,
            rx: None,
            tx: None,
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
        let msg_tx = self.tx.ok_or(MqttError::NotConnected)?;

        let reader = WasmReader {
            rx,
            buffer: self.buffer,
            connected: Arc::clone(&self.connected),
        };

        let writer = WasmWriter {
            ws,
            connected: self.connected,
            msg_tx,
            closures: Some(closures),
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

        let msg_tx_message = msg_tx.clone();
        let connected_message = self.connected.clone();
        let ws_message = ws.clone();
        let onmessage = Closure::new(move |e: MessageEvent| {
            if let Ok(abuf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                let array = js_sys::Uint8Array::new(&abuf);
                let vec = array.to_vec();
                let _ = msg_tx_message.unbounded_send(vec);
            } else {
                tracing::warn!("WebSocket received a non-binary data frame, closing connection");
                connected_message.store(false, Ordering::SeqCst);
                msg_tx_message.close_channel();
                ws_message.close().ok();
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
        let msg_tx_error = msg_tx.clone();
        let onerror = Closure::new(move |_e: ErrorEvent| {
            connected_clone2.store(false, Ordering::SeqCst);
            msg_tx_error.close_channel();
            if let Some(tx) = result_tx_error.take() {
                let _ = tx.send(Err(MqttError::ConnectionError(
                    "WebSocket connection failed".into(),
                )));
            }
        });

        let connected_clone3 = self.connected.clone();
        let msg_tx_close = msg_tx.clone();
        let onclose = Closure::new(move |_e: CloseEvent| {
            connected_clone3.store(false, Ordering::SeqCst);
            msg_tx_close.close_channel();
        });

        let closures = ClosureBundle {
            onmessage,
            onopen,
            onerror,
            onclose,
        };
        closures.attach(&ws);

        self.ws = Some(ws.clone());
        self.rx = Some(msg_rx);
        self.tx = Some(msg_tx);
        self.closures = Some(closures);

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
            self.tx = None;
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

    fn write(&mut self, buf: &[u8]) -> impl std::future::Future<Output = Result<()>> {
        let result = (|| {
            let ws = self.ws.as_ref().ok_or(MqttError::NotConnected)?;

            ws.send_with_u8_array(buf)
                .map_err(|e| MqttError::Io(format!("WebSocket send failed: {e:?}")))?;

            Ok(())
        })();
        std::future::ready(result)
    }

    fn close(&mut self) -> impl std::future::Future<Output = Result<()>> {
        if let Some(ws) = self.ws.take() {
            ws.close().ok();
        }
        self.connected.store(false, Ordering::SeqCst);
        self.closures = None;
        std::future::ready(Ok(()))
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}
