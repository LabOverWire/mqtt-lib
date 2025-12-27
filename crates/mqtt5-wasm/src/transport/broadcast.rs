use futures::channel::mpsc;
use futures::StreamExt;
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::Transport;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{BroadcastChannel, MessageEvent};

pub struct BroadcastChannelTransport {
    channel_name: String,
    channel: Option<BroadcastChannel>,
    rx: Option<mpsc::UnboundedReceiver<Vec<u8>>>,
    connected: Arc<AtomicBool>,
    closure: Option<Closure<dyn FnMut(MessageEvent)>>,
}

pub struct BroadcastChannelReader {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    connected: Arc<AtomicBool>,
}

pub struct BroadcastChannelWriter {
    channel: BroadcastChannel,
    connected: Arc<AtomicBool>,
    _closure: Closure<dyn FnMut(MessageEvent)>,
}

impl BroadcastChannelReader {
    /// # Errors
    /// Returns an error if the connection is closed or if there is no data available.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let data = self
            .rx
            .next()
            .await
            .ok_or(MqttError::ConnectionClosedByPeer)?;

        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl BroadcastChannelWriter {
    /// # Errors
    /// Returns an error if the `BroadcastChannel` send operation fails.
    pub fn write(&mut self, buf: &[u8]) -> Result<()> {
        let array = js_sys::Uint8Array::from(buf);
        self.channel
            .post_message(&array.buffer())
            .map_err(|e| MqttError::Io(format!("BroadcastChannel send failed: {e:?}")))?;
        Ok(())
    }

    /// # Errors
    /// This method does not currently return errors but uses Result for API consistency.
    pub fn close(&mut self) -> Result<()> {
        self.channel.close();
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}

impl BroadcastChannelTransport {
    #[allow(clippy::must_use_candidate)]
    pub fn new(channel_name: impl Into<String>) -> Self {
        Self {
            channel_name: channel_name.into(),
            channel: None,
            rx: None,
            connected: Arc::new(AtomicBool::new(false)),
            closure: None,
        }
    }

    /// # Errors
    /// Returns an error if the transport is not connected.
    pub fn into_split(self) -> Result<(BroadcastChannelReader, BroadcastChannelWriter)> {
        let channel = self.channel.ok_or(MqttError::NotConnected)?;
        let rx = self.rx.ok_or(MqttError::NotConnected)?;
        let closure = self.closure.ok_or(MqttError::NotConnected)?;

        let reader = BroadcastChannelReader {
            rx,
            connected: Arc::clone(&self.connected),
        };

        let writer = BroadcastChannelWriter {
            channel,
            connected: self.connected,
            _closure: closure,
        };

        Ok((reader, writer))
    }
}

impl Transport for BroadcastChannelTransport {
    async fn connect(&mut self) -> Result<()> {
        let channel = BroadcastChannel::new(&self.channel_name).map_err(|e| {
            MqttError::ConnectionError(format!("Failed to create BroadcastChannel: {e:?}"))
        })?;

        let (msg_tx, msg_rx) = mpsc::unbounded();

        let msg_tx_clone = msg_tx.clone();
        let onmessage = Closure::new(move |e: MessageEvent| {
            if let Ok(abuf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                let array = js_sys::Uint8Array::new(&abuf);
                let vec = array.to_vec();
                let _ = msg_tx_clone.unbounded_send(vec);
            }
        });

        channel.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));

        self.channel = Some(channel);
        self.rx = Some(msg_rx);
        self.closure = Some(onmessage);
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
        let channel = self.channel.as_ref().ok_or(MqttError::NotConnected)?;

        let array = js_sys::Uint8Array::from(buf);
        channel
            .post_message(&array.buffer())
            .map_err(|e| MqttError::Io(format!("BroadcastChannel send failed: {e:?}")))?;

        Ok(())
    }

    async fn close(&mut self) -> Result<()> {
        if let Some(channel) = self.channel.take() {
            channel.close();
        }
        self.connected.store(false, Ordering::SeqCst);
        self.closure = None;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
}
