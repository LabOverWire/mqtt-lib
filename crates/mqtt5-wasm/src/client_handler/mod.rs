mod connect;
mod events;
mod lifecycle;
mod publish;
mod subscribe;

use crate::broker::WasmEventCallbacks;
use crate::transport::{WasmReader, WasmWriter};
use bytes::BytesMut;
use events::{fire_event, set_prop};
use mqtt5::broker::auth::AuthProvider;
use mqtt5::broker::config::BrokerConfig;
use mqtt5::broker::resource_monitor::ResourceMonitor;
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::router::RoutableMessage;
use mqtt5::broker::storage::{ClientSession, DynamicStorage, QueueHandle};
use mqtt5::broker::sys_topics::BrokerStats;
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::MqttPacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::KeepaliveConfig;
use mqtt5_protocol::QoS;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, RwLock};
use tracing::{debug, error, info, warn};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use web_sys::MessagePort;

/// The connection's writer, shared by the packet loop and the forwarder. Each write borrows it
/// only while it writes, so neither waits on the other's packet handling.
#[derive(Clone)]
pub(crate) struct SharedWriter(Rc<RefCell<WasmWriter>>);

impl SharedWriter {
    fn new(writer: WasmWriter) -> Self {
        Self(Rc::new(RefCell::new(writer)))
    }

    fn write(&self, buf: &[u8]) -> Result<()> {
        self.0
            .try_borrow_mut()
            .map_err(|_| MqttError::ProtocolError("writer already in use".to_string()))?
            .write(buf)
    }
}

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
    /// Shared with the forwarder, which withholds what was routed for a subscription the session no
    /// longer holds.
    pub(super) session: Rc<RefCell<Option<ClientSession>>>,
    pub(super) connect_session_expiry: Option<u32>,
    pub(super) advertised_session_expiry: Option<u32>,
    disconnect_rx: Option<tokio::sync::oneshot::Receiver<mqtt5::broker::router::TakeoverNotice>>,
    qos1_rx: tokio::sync::mpsc::Receiver<RoutableMessage>,
    qos1_tx: tokio::sync::mpsc::Sender<RoutableMessage>,
    qos0_rx: tokio::sync::mpsc::Receiver<RoutableMessage>,
    qos0_tx: tokio::sync::mpsc::Sender<RoutableMessage>,
    queue: Option<QueueHandle>,
    queue_epoch: u64,
    generation: u64,
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

pub(super) struct DeliveryCheck {
    auth_provider: Arc<dyn AuthProvider>,
    client_id: String,
    user_id: Option<String>,
    session: Rc<RefCell<Option<ClientSession>>>,
}

impl DeliveryCheck {
    /// Whether `publish`, routed for the subscription `instance`, may be sent, by the native
    /// broker's rule. A `QoS` 2 delivery already started is sent again by
    /// `resend_inflight_messages` rather than the forwarder, so the DUP exemption only keeps the
    /// rule the same.
    pub(super) async fn allows(&self, publish: &PublishPacket, instance: Option<u64>) -> bool {
        if publish.dup && publish.packet_id.is_some() {
            return true;
        }
        if self
            .session
            .borrow()
            .as_ref()
            .is_some_and(|session| !session.delivers(instance, &publish.topic_name))
        {
            return false;
        }
        !self.auth_provider.checks_delivery()
            || self
                .auth_provider
                .authorize_delivery(
                    &self.client_id,
                    self.user_id.as_deref(),
                    &publish.topic_name,
                )
                .await
    }
}

impl WasmClientHandler {
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

        let (qos1_tx, qos1_rx) = tokio::sync::mpsc::channel(100);
        let (qos0_tx, qos0_rx) = tokio::sync::mpsc::channel(100);
        let handler_id = HANDLER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let (msg_tx, msg_rx) = mpsc::unbounded();
        let msg_tx_clone = msg_tx.clone();

        let handler_fn = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::MessageEvent)>::new(
            move |e: web_sys::MessageEvent| {
                if let Ok(abuf) = e.data().dyn_into::<js_sys::ArrayBuffer>() {
                    let array = js_sys::Uint8Array::new(&abuf);
                    if msg_tx_clone.unbounded_send(array.to_vec()).is_err() {
                        debug!("Dropping message received after the client port closed");
                    }
                }
            },
        );
        let js_fn: js_sys::Function = handler_fn.into_js_value().unchecked_into();
        if let Err(e) = port.add_event_listener_with_callback("message", &js_fn) {
            error!("Failed to listen for client port messages: {e:?}");
        }
        let close_fn = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::Event)>::new(
            move |_: web_sys::Event| {
                debug!("Client port closed");
                msg_tx.close_channel();
            },
        );
        let close_js: js_sys::Function = close_fn.into_js_value().unchecked_into();
        if let Err(e) = port.add_event_listener_with_callback("close", &close_js) {
            error!("Failed to listen for client port close: {e:?}");
        }
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
            session: Rc::new(RefCell::new(None)),
            connect_session_expiry: None,
            advertised_session_expiry: None,
            disconnect_rx: None,
            qos1_rx,
            qos1_tx,
            qos0_rx,
            qos0_tx,
            queue: None,
            queue_epoch: 0,
            generation: 0,
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
        let writer = SharedWriter::new(WasmWriter::MessagePort(writer));

        let handshake = self.wait_for_connect(&mut reader, &writer).await;
        let (Some(client_id), Some(disconnect_rx), Some(queue)) = (
            self.client_id.clone().filter(|_| self.generation != 0),
            self.disconnect_rx.take(),
            self.queue.clone(),
        ) else {
            return handshake;
        };
        if let Err(e) = handshake {
            self.release_ownership(&client_id).await;
            return Err(e);
        }
        queue.notify();

        self.stats.client_connected();

        let result = self.packet_loop(&mut reader, writer, disconnect_rx).await;

        let armed_will = self.release_ownership(&client_id).await;
        let (reason, unexpected) = if self.normal_disconnect {
            ("client disconnected", false)
        } else {
            self.publish_will_message(&client_id, armed_will).await;
            ("connection lost", true)
        };

        self.fire_client_disconnect(&client_id, reason, unexpected);
        self.stats.client_disconnected();

        result
    }

    fn spawn_disconnect_watcher(
        running: &Rc<RefCell<bool>>,
        disconnect_rx: tokio::sync::oneshot::Receiver<mqtt5::broker::router::TakeoverNotice>,
    ) {
        let running_clone = Rc::clone(running);
        spawn_local(async move {
            match disconnect_rx.await {
                Ok(_notice) => {
                    info!("Session takeover signal received");
                    *running_clone.borrow_mut() = false;
                }
                Err(_) => {
                    debug!("Disconnect sender dropped");
                }
            }
        });
    }

    pub(super) fn delivery_check(&self) -> DeliveryCheck {
        DeliveryCheck {
            auth_provider: Arc::clone(&self.auth_provider),
            client_id: self.client_id.clone().unwrap_or_default(),
            user_id: self.user_id.clone(),
            session: Rc::clone(&self.session),
        }
    }

    async fn forward_one(
        mut publish: PublishPacket,
        writer: &SharedWriter,
        outbound_inflight: &Rc<RefCell<HashMap<u16, PublishPacket>>>,
        next_pid: &Rc<Cell<u16>>,
        storage: &Arc<DynamicStorage>,
        client_id: &str,
        handler_id: u32,
    ) -> bool {
        use mqtt5::broker::storage::{
            InflightDirection, InflightMessage, InflightPhase, StorageBackend,
        };

        if publish.qos != QoS::AtMostOnce {
            let pid = next_pid.get();
            next_pid.set(if pid == u16::MAX { 1 } else { pid + 1 });
            publish.packet_id = Some(pid);

            if publish.qos == QoS::ExactlyOnce {
                outbound_inflight.borrow_mut().insert(pid, publish.clone());
                let inflight = InflightMessage::from_publish(
                    &publish,
                    client_id.to_string(),
                    InflightDirection::Outbound,
                    InflightPhase::AwaitingPubrec,
                );
                if let Err(e) = storage.store_inflight_message(inflight).await {
                    tracing::debug!("failed to persist outbound inflight {pid}: {e}");
                }
            }
        }

        if let Err(e) = Self::write_publish_packet(&publish, writer) {
            error!("Handler #{handler_id} error forwarding publish: {e}");
            return false;
        }
        true
    }

    fn spawn_publish_forwarder(
        &mut self,
        running: &Rc<RefCell<bool>>,
        writer_shared: &SharedWriter,
    ) {
        let running_forward = Rc::clone(running);
        let mut qos1_rx = std::mem::replace(&mut self.qos1_rx, tokio::sync::mpsc::channel(1).1);
        let mut qos0_rx = std::mem::replace(&mut self.qos0_rx, tokio::sync::mpsc::channel(1).1);
        let queue = self.queue.clone();
        let queue_epoch = self.queue_epoch;
        let writer_for_forward = writer_shared.clone();
        let outbound_inflight_fwd = Rc::clone(&self.outbound_inflight);
        let next_pid_fwd = Rc::clone(&self.next_packet_id);
        let storage_fwd = Arc::clone(&self.storage);
        let client_id_fwd = self.client_id.clone().unwrap_or_default();
        let delivery_check = self.delivery_check();
        let handler_id = self.handler_id;
        let protocol_version = self.protocol_version;

        spawn_local(async move {
            'outer: loop {
                if !*running_forward.borrow() {
                    break;
                }

                let RoutableMessage {
                    mut publish,
                    subscription_instance,
                    ..
                } = tokio::select! {
                    lane = qos1_rx.recv() => match lane {
                        Some(routable) => routable,
                        None => break,
                    },
                    lane = qos0_rx.recv() => match lane {
                        Some(routable) => routable,
                        None => break,
                    },
                    () = async {
                        match &queue {
                            Some(queue) => queue.notified().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        if let Some(queue) = &queue {
                            let mut pending = queue.take(64).await.into_iter();
                            while let Some(message) = pending.next() {
                                let mut queued = message.to_publish_packet();
                                if !delivery_check
                                    .allows(&queued, message.subscription_instance)
                                    .await
                                {
                                    continue;
                                }
                                queued.protocol_version = protocol_version;
                                if !Self::forward_one(
                                    queued,
                                    &writer_for_forward,
                                    &outbound_inflight_fwd,
                                    &next_pid_fwd,
                                    &storage_fwd,
                                    &client_id_fwd,
                                    handler_id,
                                )
                                .await
                                {
                                    let mut rest = vec![message];
                                    rest.extend(pending);
                                    queue.requeue_front_in_epoch(rest, queue_epoch);
                                    queue.finish_drain();
                                    break 'outer;
                                }
                            }
                            queue.finish_drain();
                            if queue.count() > 0 {
                                queue.notify();
                            }
                        }
                        continue;
                    }
                };

                if !delivery_check.allows(&publish, subscription_instance).await {
                    continue;
                }
                publish.protocol_version = protocol_version;
                if !Self::forward_one(
                    publish,
                    &writer_for_forward,
                    &outbound_inflight_fwd,
                    &next_pid_fwd,
                    &storage_fwd,
                    &client_id_fwd,
                    handler_id,
                )
                .await
                {
                    break;
                }
            }
        });
    }

    fn spawn_keepalive_timer(
        running: &Rc<RefCell<bool>>,
        last_packet_time: &Rc<Cell<mqtt5::time::Instant>>,
        keep_alive: mqtt5::time::Duration,
        handler_id: u32,
        timeout_tx: tokio::sync::mpsc::Sender<()>,
    ) {
        let running_timeout = Rc::clone(running);
        let last_packet_time_timeout = Rc::clone(last_packet_time);
        let timeout_duration = KeepaliveConfig::default().timeout_duration(keep_alive);
        spawn_local(async move {
            loop {
                let remaining =
                    timeout_duration.saturating_sub(last_packet_time_timeout.get().elapsed());
                if remaining.is_zero() {
                    if *running_timeout.borrow() {
                        warn!("Handler #{handler_id} keep-alive timeout detected");
                        *running_timeout.borrow_mut() = false;
                        let _ = timeout_tx.send(()).await;
                    }
                    break;
                }

                gloo_timers::future::sleep(remaining).await;

                if !*running_timeout.borrow() {
                    break;
                }
            }
        });
    }

    async fn packet_loop(
        &mut self,
        reader: &mut WasmReader,
        writer: SharedWriter,
        disconnect_rx: tokio::sync::oneshot::Receiver<mqtt5::broker::router::TakeoverNotice>,
    ) -> Result<()> {
        use crate::decoder::read_packet;

        let running = Rc::new(RefCell::new(true));
        Self::spawn_disconnect_watcher(&running, disconnect_rx);

        let last_packet_time = Rc::new(Cell::new(mqtt5::time::Instant::now()));
        self.spawn_publish_forwarder(&running, &writer);

        let (timeout_tx, mut timeout_rx) = tokio::sync::mpsc::channel::<()>(1);
        if !self.keep_alive.is_zero() {
            Self::spawn_keepalive_timer(
                &running,
                &last_packet_time,
                self.keep_alive,
                self.handler_id,
                timeout_tx,
            );
        }

        loop {
            if !*running.borrow() {
                info!("Session takeover or keep-alive timeout, disconnecting");
                return Ok(());
            }

            let packet_future = read_packet(reader, self.protocol_version);
            futures::pin_mut!(packet_future);
            let timeout_future = timeout_rx.recv();
            futures::pin_mut!(timeout_future);

            match futures::future::select(packet_future, timeout_future).await {
                futures::future::Either::Left((packet_result, _)) => match packet_result {
                    Ok(packet) => {
                        last_packet_time.set(mqtt5::time::Instant::now());
                        if let Err(e) = self.handle_packet(packet, &writer).await {
                            error!("Error handling packet: {e}");
                            return Err(e);
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

    async fn handle_packet(&mut self, packet: Packet, writer: &SharedWriter) -> Result<()> {
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
            Packet::Disconnect(ref disconnect) => self.handle_disconnect(disconnect, writer).await,
            Packet::Auth(auth) => self.handle_auth(auth, writer).await,
            _ => {
                warn!("Unexpected packet type");
                Ok(())
            }
        }
    }

    #[allow(clippy::unused_self)]
    pub(super) fn write_packet(&self, packet: &Packet, writer: &SharedWriter) -> Result<()> {
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
