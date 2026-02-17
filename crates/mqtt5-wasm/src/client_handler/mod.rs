mod connect;
mod events;
mod lifecycle;
mod publish;
mod subscribe;

use crate::broker::WasmEventCallbacks;
use crate::transport::message_port::MessagePortTransport;
use crate::transport::{WasmReader, WasmWriter};
use bytes::BytesMut;
use events::{fire_event, set_prop};
use mqtt5::broker::auth::AuthProvider;
use mqtt5::broker::config::BrokerConfig;
use mqtt5::broker::resource_monitor::ResourceMonitor;
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::storage::{ClientSession, DynamicStorage};
use mqtt5::broker::sys_topics::BrokerStats;
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::KeepaliveConfig;
use mqtt5_protocol::QoS;
use mqtt5_protocol::Transport;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use tracing::{debug, error, info, warn};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use web_sys::MessagePort;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum AuthState {
    NotStarted,
    InProgress,
    Completed,
}

pub(super) struct PendingConnect {
    pub connect: mqtt5_protocol::packet::connect::ConnectPacket,
    pub assigned_client_id: Option<String>,
}

pub struct WasmClientHandler {
    handler_id: u32,
    pub(super) client_id: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) protocol_version: u8,
    pub(super) config: Arc<RwLock<BrokerConfig>>,
    pub(super) router: Arc<MessageRouter>,
    pub(super) auth_provider: Arc<dyn AuthProvider>,
    pub(super) storage: Arc<DynamicStorage>,
    pub(super) stats: Arc<BrokerStats>,
    pub(super) resource_monitor: Arc<ResourceMonitor>,
    pub(super) session: Option<ClientSession>,
    publish_rx: flume::Receiver<PublishPacket>,
    publish_tx: flume::Sender<PublishPacket>,
    pub(super) inflight_publishes: HashMap<u16, PublishPacket>,
    pub(super) outbound_inflight: Rc<RefCell<HashMap<u16, PublishPacket>>>,
    pub(super) next_packet_id: Rc<Cell<u16>>,
    pub(super) normal_disconnect: bool,
    pub(super) keep_alive: mqtt5::time::Duration,
    pub(super) auth_method: Option<String>,
    pub(super) auth_state: AuthState,
    pub(super) pending_connect: Option<PendingConnect>,
    event_callbacks: WasmEventCallbacks,
}

static HANDLER_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

impl WasmClientHandler {
    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn start_deferred(
        port: MessagePort,
        config: Arc<RwLock<BrokerConfig>>,
        router: Arc<MessageRouter>,
        auth_provider: Arc<dyn AuthProvider>,
        storage: Arc<DynamicStorage>,
        stats: Arc<BrokerStats>,
        resource_monitor: Arc<ResourceMonitor>,
        event_callbacks: WasmEventCallbacks,
    ) {
        use wasm_bindgen::JsCast;

        let port = Rc::new(RefCell::new(Some(port)));
        let port_clone = Rc::clone(&port);

        let config = Rc::new(config);
        let router = Rc::new(router);
        let auth_provider: Rc<Arc<dyn AuthProvider>> = Rc::new(auth_provider);
        let storage = Rc::new(storage);
        let stats = Rc::new(stats);
        let resource_monitor = Rc::new(resource_monitor);
        let event_callbacks = Rc::new(event_callbacks);

        let callback = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || {
            if let Some(p) = port_clone.borrow_mut().take() {
                let config = (*config).clone();
                let router = (*router).clone();
                let auth_provider = (*auth_provider).clone();
                let storage = (*storage).clone();
                let stats = (*stats).clone();
                let resource_monitor = (*resource_monitor).clone();
                let event_callbacks = (*event_callbacks).clone();

                Self::new(
                    p,
                    config,
                    router,
                    auth_provider,
                    storage,
                    stats,
                    resource_monitor,
                    event_callbacks,
                );
            }
        });

        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback(callback.as_ref().unchecked_ref());
        }
        callback.forget();
    }

    #[allow(
        clippy::must_use_candidate,
        clippy::new_ret_no_self,
        clippy::too_many_arguments
    )]
    pub fn new(
        port: MessagePort,
        config: Arc<RwLock<BrokerConfig>>,
        router: Arc<MessageRouter>,
        auth_provider: Arc<dyn AuthProvider>,
        storage: Arc<DynamicStorage>,
        stats: Arc<BrokerStats>,
        resource_monitor: Arc<ResourceMonitor>,
        event_callbacks: WasmEventCallbacks,
    ) {
        use futures::channel::mpsc;
        use wasm_bindgen::JsCast;

        let (publish_tx, publish_rx) = flume::bounded(100);
        let handler_id = HANDLER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let (msg_tx, msg_rx) = mpsc::unbounded();
        let msg_tx_clone = msg_tx.clone();

        let handler_fn = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |e: web_sys::MessageEvent| {
                if let Ok(abuf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                    let array = js_sys::Uint8Array::new(&abuf);
                    let vec = array.to_vec();
                    let _ = msg_tx_clone.unbounded_send(vec);
                }
            },
        );
        let js_fn: js_sys::Function = handler_fn.into_js_value().unchecked_into();
        let _ = port.add_event_listener_with_callback("message", &js_fn);
        port.start();

        let handler = Self {
            handler_id,
            client_id: None,
            user_id: None,
            protocol_version: 5,
            config,
            router,
            auth_provider,
            storage,
            stats,
            resource_monitor,
            session: None,
            publish_rx,
            publish_tx,
            inflight_publishes: HashMap::new(),
            outbound_inflight: Rc::new(RefCell::new(HashMap::new())),
            next_packet_id: Rc::new(Cell::new(1)),
            normal_disconnect: false,
            keep_alive: mqtt5::time::Duration::from_secs(60),
            auth_method: None,
            auth_state: AuthState::NotStarted,
            pending_connect: None,
            event_callbacks,
        };

        spawn_local(async move {
            if let Err(e) = handler.run_with_receiver(port, msg_rx).await {
                error!("Client handler error: {e}");
            }
        });
    }

    #[allow(dead_code)]
    async fn run(mut self, port: MessagePort) -> Result<()> {
        let mut transport = MessagePortTransport::new(port);
        transport.connect().await?;

        let (reader, writer) = transport.into_split()?;
        let mut reader = WasmReader::MessagePort(reader);
        let mut writer = WasmWriter::MessagePort(writer);

        self.wait_for_connect(&mut reader, &mut writer).await?;

        let client_id = self.client_id.clone().unwrap();
        let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel();

        self.router
            .register_client(client_id.clone(), self.publish_tx.clone(), disconnect_tx)
            .await;

        self.stats.client_connected();

        let result = self.packet_loop(&mut reader, writer, disconnect_rx).await;

        if !self.normal_disconnect {
            self.publish_will_message(&client_id).await;
        }

        self.router.unregister_client(&client_id).await;
        self.stats.client_disconnected();

        result
    }

    async fn run_with_receiver(
        mut self,
        port: MessagePort,
        msg_rx: futures::channel::mpsc::UnboundedReceiver<Vec<u8>>,
    ) -> Result<()> {
        use crate::transport::message_port::{MessagePortReader, MessagePortWriter};
        use std::sync::{atomic::AtomicBool, Arc};

        let connected = Arc::new(AtomicBool::new(true));

        let reader = MessagePortReader::new(msg_rx, Arc::clone(&connected));
        let writer = MessagePortWriter::new(port, connected, None);

        let mut reader = WasmReader::MessagePort(reader);
        let mut writer = WasmWriter::MessagePort(writer);

        self.wait_for_connect(&mut reader, &mut writer).await?;

        let client_id = self.client_id.clone().unwrap();
        let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel();

        self.router
            .register_client(client_id.clone(), self.publish_tx.clone(), disconnect_tx)
            .await;

        self.stats.client_connected();

        let result = self.packet_loop(&mut reader, writer, disconnect_rx).await;

        let (reason, unexpected) = if self.normal_disconnect {
            ("client disconnected", false)
        } else {
            self.publish_will_message(&client_id).await;
            ("connection lost", true)
        };

        self.fire_client_disconnect(&client_id, reason, unexpected);

        self.router.unregister_client(&client_id).await;
        self.stats.client_disconnected();

        result
    }

    #[allow(clippy::await_holding_refcell_ref, clippy::too_many_lines)]
    async fn packet_loop(
        &mut self,
        reader: &mut WasmReader,
        writer: WasmWriter,
        disconnect_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        use crate::decoder::read_packet;

        let running = Rc::new(RefCell::new(true));
        let running_clone = Rc::clone(&running);

        spawn_local(async move {
            match disconnect_rx.await {
                Ok(()) => {
                    info!("Session takeover signal received");
                    *running_clone.borrow_mut() = false;
                }
                Err(_) => {
                    debug!("Disconnect sender dropped");
                }
            }
        });

        let last_packet_time = Rc::new(Cell::new(mqtt5::time::Instant::now()));
        let keep_alive = self.keep_alive;
        let handler_id = self.handler_id;

        let running_forward = Rc::clone(&running);
        let publish_rx = std::mem::replace(&mut self.publish_rx, flume::bounded(1).1);
        let writer_shared = Rc::new(RefCell::new(writer));
        let writer_for_forward = Rc::clone(&writer_shared);
        let outbound_inflight_fwd = Rc::clone(&self.outbound_inflight);
        let next_pid_fwd = Rc::clone(&self.next_packet_id);
        let storage_fwd = Arc::clone(&self.storage);
        let client_id_fwd = self.client_id.clone().unwrap_or_default();

        spawn_local(async move {
            use mqtt5::broker::storage::{
                InflightDirection, InflightMessage, InflightPhase, StorageBackend,
            };

            loop {
                if !*running_forward.borrow() {
                    break;
                }

                match publish_rx.recv_async().await {
                    Ok(mut publish) => {
                        if publish.qos != QoS::AtMostOnce {
                            let pid = next_pid_fwd.get();
                            next_pid_fwd.set(if pid == u16::MAX { 1 } else { pid + 1 });
                            publish.packet_id = Some(pid);

                            if publish.qos == QoS::ExactlyOnce {
                                outbound_inflight_fwd
                                    .borrow_mut()
                                    .insert(pid, publish.clone());
                                let inflight = InflightMessage::from_publish(
                                    &publish,
                                    client_id_fwd.clone(),
                                    InflightDirection::Outbound,
                                    InflightPhase::AwaitingPubrec,
                                );
                                if let Err(e) = storage_fwd.store_inflight_message(inflight).await {
                                    tracing::debug!(
                                        "failed to persist outbound inflight {pid}: {e}"
                                    );
                                }
                            }
                        }

                        if let Ok(mut writer_guard) = writer_for_forward.try_borrow_mut() {
                            let result = Self::write_publish_packet(&publish, &mut writer_guard);
                            if let Err(e) = result {
                                error!("Handler #{handler_id} error forwarding publish: {e}");
                                break;
                            }
                        } else {
                            error!("Handler #{handler_id} writer busy, cannot forward");
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let (timeout_tx, mut timeout_rx) = tokio::sync::mpsc::channel::<()>(1);

        if !keep_alive.is_zero() {
            let running_timeout = Rc::clone(&running);
            let last_packet_time_timeout = Rc::clone(&last_packet_time);
            let timeout_duration = KeepaliveConfig::default().timeout_duration(keep_alive);
            spawn_local(async move {
                loop {
                    gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;

                    if !*running_timeout.borrow() {
                        break;
                    }

                    let elapsed = last_packet_time_timeout.get().elapsed();
                    if elapsed > timeout_duration {
                        warn!("Handler #{} keep-alive timeout detected", handler_id);
                        *running_timeout.borrow_mut() = false;
                        let _ = timeout_tx.send(()).await;
                        break;
                    }
                }
            });
        }

        loop {
            if !*running.borrow() {
                info!("Session takeover or keep-alive timeout, disconnecting");
                return Ok(());
            }

            let packet_future = read_packet(reader);
            futures::pin_mut!(packet_future);
            let timeout_future = timeout_rx.recv();
            futures::pin_mut!(timeout_future);

            match futures::future::select(packet_future, timeout_future).await {
                futures::future::Either::Left((packet_result, _)) => match packet_result {
                    Ok(packet) => {
                        last_packet_time.set(mqtt5::time::Instant::now());
                        if let Ok(mut writer_guard) = writer_shared.try_borrow_mut() {
                            if let Err(e) = self.handle_packet(packet, &mut writer_guard).await {
                                error!("Error handling packet: {e}");
                                return Err(e);
                            }
                        } else {
                            error!("Handler writer busy in main loop");
                        }
                    }
                    Err(e) => {
                        debug!("Connection closed: {e}");
                        return Ok(());
                    }
                },
                futures::future::Either::Right((_, _)) => {
                    info!("Keep-alive timeout signal received");
                    return Ok(());
                }
            }
        }
    }

    async fn handle_packet(&mut self, packet: Packet, writer: &mut WasmWriter) -> Result<()> {
        match packet {
            Packet::Subscribe(subscribe) => self.handle_subscribe(subscribe, writer).await,
            Packet::Unsubscribe(unsubscribe) => self.handle_unsubscribe(unsubscribe, writer).await,
            Packet::Publish(publish) => self.handle_publish(publish, writer).await,
            Packet::PubAck(ref puback) => {
                self.handle_puback(puback);
                Ok(())
            }
            Packet::PubRec(pubrec) => self.handle_pubrec(&pubrec, writer).await,
            Packet::PubRel(pubrel) => self.handle_pubrel(pubrel, writer).await,
            Packet::PubComp(ref pubcomp) => {
                self.handle_pubcomp(pubcomp).await;
                Ok(())
            }
            Packet::PingReq => self.handle_pingreq(writer),
            Packet::Disconnect(ref disconnect) => self.handle_disconnect(disconnect),
            Packet::Auth(auth) => self.handle_auth(auth, writer).await,
            _ => {
                warn!("Unexpected packet type");
                Ok(())
            }
        }
    }

    #[allow(clippy::unused_self)]
    pub(super) fn write_packet(&self, packet: &Packet, writer: &mut WasmWriter) -> Result<()> {
        let mut buf = BytesMut::new();

        match packet {
            Packet::ConnAck(p) => p.encode(&mut buf)?,
            Packet::SubAck(p) => p.encode(&mut buf)?,
            Packet::UnsubAck(p) => p.encode(&mut buf)?,
            Packet::Publish(p) => p.encode(&mut buf)?,
            Packet::PubAck(p) => p.encode(&mut buf)?,
            Packet::PubRec(p) => p.encode(&mut buf)?,
            Packet::PubRel(p) => p.encode(&mut buf)?,
            Packet::PubComp(p) => p.encode(&mut buf)?,
            Packet::PingResp => {
                mqtt5_protocol::packet::pingresp::PingRespPacket::default().encode(&mut buf)?;
            }
            Packet::Disconnect(p) => p.encode(&mut buf)?,
            Packet::Auth(p) => p.encode(&mut buf)?,
            _ => {
                return Err(MqttError::ProtocolError(format!(
                    "Encoding not yet implemented for packet type: {packet:?}"
                )));
            }
        }

        writer.write(&buf)?;
        Ok(())
    }

    pub(super) fn advance_packet_id_past_inflight(&self) {
        let outbound = self.outbound_inflight.borrow();
        let mut candidate = self.next_packet_id.get();
        for _ in 0..u16::MAX {
            if !outbound.contains_key(&candidate)
                && !self.inflight_publishes.contains_key(&candidate)
            {
                self.next_packet_id.set(candidate);
                return;
            }
            candidate = if candidate == u16::MAX {
                1
            } else {
                candidate + 1
            };
        }
    }

    pub(super) fn fire_client_connect(&self, client_id: &str, clean_start: bool) {
        let callback = self.event_callbacks.on_client_connect.borrow().clone();
        let client_id = client_id.to_string();
        fire_event(callback, "on_client_connect", move |obj| {
            set_prop(obj, "clientId", &client_id.into());
            set_prop(obj, "cleanStart", &clean_start.into());
        });
    }

    pub(super) fn fire_client_disconnect(&self, client_id: &str, reason: &str, unexpected: bool) {
        let callback = self.event_callbacks.on_client_disconnect.borrow().clone();
        let client_id = client_id.to_string();
        let reason = reason.to_string();
        fire_event(callback, "on_client_disconnect", move |obj| {
            set_prop(obj, "clientId", &client_id.into());
            set_prop(obj, "reason", &reason.into());
            set_prop(obj, "unexpected", &unexpected.into());
        });
    }

    pub(super) fn fire_client_publish(
        &self,
        client_id: &str,
        topic: &str,
        qos: u8,
        retain: bool,
        payload_size: usize,
    ) {
        let callback = self.event_callbacks.on_client_publish.borrow().clone();
        let client_id = client_id.to_string();
        let topic = topic.to_string();
        let safe_size = crate::utils::usize_to_f64_saturating(payload_size);
        fire_event(callback, "on_client_publish", move |obj| {
            set_prop(obj, "clientId", &client_id.into());
            set_prop(obj, "topic", &topic.into());
            set_prop(obj, "qos", &JsValue::from_f64(f64::from(qos)));
            set_prop(obj, "retain", &retain.into());
            set_prop(obj, "payloadSize", &JsValue::from_f64(safe_size));
        });
    }

    pub(super) fn fire_client_subscribe(&self, client_id: &str, subscriptions: &[(String, u8)]) {
        let callback = self.event_callbacks.on_client_subscribe.borrow().clone();
        let client_id = client_id.to_string();
        let subscriptions = subscriptions.to_vec();
        fire_event(callback, "on_client_subscribe", move |obj| {
            set_prop(obj, "clientId", &client_id.into());
            let subs_array = js_sys::Array::new();
            for (topic, qos) in &subscriptions {
                let sub_obj = js_sys::Object::new();
                set_prop(&sub_obj, "topic", &topic.into());
                set_prop(&sub_obj, "qos", &JsValue::from_f64(f64::from(*qos)));
                subs_array.push(&sub_obj);
            }
            set_prop(obj, "subscriptions", &subs_array);
        });
    }

    pub(super) fn fire_client_unsubscribe(&self, client_id: &str, topics: &[String]) {
        let callback = self.event_callbacks.on_client_unsubscribe.borrow().clone();
        let client_id = client_id.to_string();
        let topics = topics.to_vec();
        fire_event(callback, "on_client_unsubscribe", move |obj| {
            set_prop(obj, "clientId", &client_id.into());
            let topics_array = js_sys::Array::new();
            for topic in &topics {
                topics_array.push(&JsValue::from_str(topic));
            }
            set_prop(obj, "topics", &topics_array);
        });
    }

    pub(super) fn fire_message_delivered(&self, client_id: &str, packet_id: u16, qos: u8) {
        let callback = self.event_callbacks.on_message_delivered.borrow().clone();
        let client_id = client_id.to_string();
        fire_event(callback, "on_message_delivered", move |obj| {
            set_prop(obj, "clientId", &client_id.into());
            set_prop(obj, "packetId", &JsValue::from_f64(f64::from(packet_id)));
            set_prop(obj, "qos", &JsValue::from_f64(f64::from(qos)));
        });
    }
}
