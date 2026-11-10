mod connect;
mod events;
mod forwarder;
mod lifecycle;
mod outbound;
mod publish;
mod subscribe;

use crate::broker::WasmEventCallbacks;
use crate::transport::{WasmReader, WasmWriter};
use bytes::BytesMut;
use events::{fire_event, set_prop};
use forwarder::Forwarder;
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
use outbound::OutboundInflight;
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
    released_rx: Option<tokio::sync::oneshot::Receiver<()>>,
    resumed: bool,
    qos1_rx: tokio::sync::mpsc::Receiver<RoutableMessage>,
    qos1_tx: tokio::sync::mpsc::Sender<RoutableMessage>,
    qos0_rx: tokio::sync::mpsc::Receiver<RoutableMessage>,
    qos0_tx: tokio::sync::mpsc::Sender<RoutableMessage>,
    queue: Option<QueueHandle>,
    queue_epoch: u64,
    generation: u64,
    pub(super) inflight_publishes: HashMap<u16, PublishPacket>,
    outbound_inflight: Rc<RefCell<OutboundInflight>>,
    pub(super) next_packet_id: Rc<Cell<u16>>,
    pub(super) client_receive_maximum: u16,
    pub(super) client_max_packet_size: Option<u32>,
    pub(super) acknowledged: Rc<tokio::sync::Notify>,
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
    session: Rc<RefCell<Option<ClientSession>>>,
}

/// Whether the connection is still served. The forwarder waits on `stopped` to hand the session's
/// messages back.
pub(super) struct RunState {
    running: Cell<bool>,
    stop: tokio::sync::Notify,
}

impl RunState {
    fn new() -> Self {
        Self {
            running: Cell::new(true),
            stop: tokio::sync::Notify::new(),
        }
    }

    pub(super) fn is_running(&self) -> bool {
        self.running.get()
    }

    pub(super) fn stop(&self) {
        if self.running.replace(false) {
            self.stop.notify_one();
        }
    }

    pub(super) async fn stopped(&self) {
        if self.running.get() {
            self.stop.notified().await;
        }
    }
}

/// Why a connection's packet loop was told to stop while it waited for a packet.
enum StopReason {
    KeepAliveTimeout,
    TakenOver,
}

impl DeliveryCheck {
    /// Whether `publish`, routed for the subscription `instance`, may be sent, by the native
    /// broker's rule, for the session's current user. A delivery already started, sent again with
    /// DUP and its packet identifier, is completed.
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
        if !self.auth_provider.checks_delivery() {
            return true;
        }
        let user_id = self
            .session
            .borrow()
            .as_ref()
            .and_then(|session| session.user_id.clone());
        self.auth_provider
            .authorize_delivery(&self.client_id, user_id.as_deref(), &publish.topic_name)
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
            released_rx: None,
            resumed: false,
            qos1_rx,
            qos1_tx,
            qos0_rx,
            qos0_tx,
            queue: None,
            queue_epoch: 0,
            generation: 0,
            inflight_publishes: HashMap::new(),
            outbound_inflight: Rc::new(RefCell::new(OutboundInflight::default())),
            next_packet_id: Rc::new(Cell::new(1)),
            client_receive_maximum: u16::MAX,
            client_max_packet_size: None,
            acknowledged: Rc::new(tokio::sync::Notify::new()),
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
        self.stats.client_connected();

        let result = self
            .packet_loop(&mut reader, writer, disconnect_rx, queue)
            .await;

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
        run_state: &Rc<RunState>,
        disconnect_rx: tokio::sync::oneshot::Receiver<mqtt5::broker::router::TakeoverNotice>,
        notice_tx: tokio::sync::oneshot::Sender<mqtt5::broker::router::TakeoverNotice>,
        stop_tx: tokio::sync::mpsc::Sender<StopReason>,
    ) {
        let run_state = Rc::clone(run_state);
        spawn_local(async move {
            match disconnect_rx.await {
                Ok(notice) => {
                    info!("Session takeover signal received");
                    run_state.stop();
                    if notice_tx.send(notice).is_err() {
                        debug!("Forwarder already handed the session back");
                    }
                    let _ = stop_tx.send(StopReason::TakenOver).await;
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
            session: Rc::clone(&self.session),
        }
    }

    fn spawn_publish_forwarder(
        &mut self,
        run_state: &Rc<RunState>,
        writer_shared: &SharedWriter,
        queue: QueueHandle,
        notice_rx: tokio::sync::oneshot::Receiver<mqtt5::broker::router::TakeoverNotice>,
        done_tx: tokio::sync::oneshot::Sender<()>,
    ) {
        let forwarder = Forwarder {
            run_state: Rc::clone(run_state),
            writer: writer_shared.clone(),
            outbound_inflight: Rc::clone(&self.outbound_inflight),
            next_pid: Rc::clone(&self.next_packet_id),
            storage: Arc::clone(&self.storage),
            client_id: self.client_id.clone().unwrap_or_default(),
            handler_id: self.handler_id,
            delivery_check: self.delivery_check(),
            protocol_version: self.protocol_version,
            queue,
            queue_epoch: self.queue_epoch,
            generation: self.generation,
            resumed: self.resumed,
            released: self.released_rx.take(),
            notice_rx,
            done_tx,
            qos1_rx: std::mem::replace(&mut self.qos1_rx, tokio::sync::mpsc::channel(1).1),
            qos0_rx: std::mem::replace(&mut self.qos0_rx, tokio::sync::mpsc::channel(1).1),
            receive_maximum: usize::from(self.client_receive_maximum),
            max_packet_size: self
                .client_max_packet_size
                .map(|max| usize::try_from(max).unwrap_or(usize::MAX)),
            acknowledged: Rc::clone(&self.acknowledged),
            reserved_pids: std::collections::HashSet::new(),
        };
        spawn_local(forwarder.run());
    }

    fn spawn_keepalive_timer(
        run_state: &Rc<RunState>,
        last_packet_time: &Rc<Cell<mqtt5::time::Instant>>,
        keep_alive: mqtt5::time::Duration,
        handler_id: u32,
        stop_tx: tokio::sync::mpsc::Sender<StopReason>,
    ) {
        let run_state = Rc::clone(run_state);
        let last_packet_time_timeout = Rc::clone(last_packet_time);
        let timeout_duration = KeepaliveConfig::default().timeout_duration(keep_alive);
        spawn_local(async move {
            loop {
                let remaining =
                    timeout_duration.saturating_sub(last_packet_time_timeout.get().elapsed());
                if remaining.is_zero() {
                    if run_state.is_running() {
                        warn!("Handler #{handler_id} keep-alive timeout detected");
                        run_state.stop();
                        let _ = stop_tx.send(StopReason::KeepAliveTimeout).await;
                    }
                    break;
                }

                gloo_timers::future::sleep(remaining).await;

                if !run_state.is_running() {
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
        queue: QueueHandle,
    ) -> Result<()> {
        let run_state = Rc::new(RunState::new());
        let (stop_tx, stop_rx) = tokio::sync::mpsc::channel(2);
        let (notice_tx, notice_rx) = tokio::sync::oneshot::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        Self::spawn_disconnect_watcher(&run_state, disconnect_rx, notice_tx, stop_tx.clone());

        let last_packet_time = Rc::new(Cell::new(mqtt5::time::Instant::now()));
        self.spawn_publish_forwarder(&run_state, &writer, queue, notice_rx, done_tx);

        if !self.keep_alive.is_zero() {
            Self::spawn_keepalive_timer(
                &run_state,
                &last_packet_time,
                self.keep_alive,
                self.handler_id,
                stop_tx,
            );
        }

        let result = self
            .read_packets(reader, &writer, &last_packet_time, stop_rx)
            .await;
        run_state.stop();
        if done_rx.await.is_err() {
            debug!("Forwarder ended without handing the session back");
        }
        result
    }

    async fn read_packets(
        &mut self,
        reader: &mut WasmReader,
        writer: &SharedWriter,
        last_packet_time: &Cell<mqtt5::time::Instant>,
        mut stop_rx: tokio::sync::mpsc::Receiver<StopReason>,
    ) -> Result<()> {
        use crate::decoder::read_packet;
        use futures::future::Either;

        loop {
            let event = {
                let packet_future = read_packet(reader, self.protocol_version);
                futures::pin_mut!(packet_future);
                let stop_future = async {
                    match stop_rx.recv().await {
                        Some(stop) => stop,
                        None => std::future::pending().await,
                    }
                };
                futures::pin_mut!(stop_future);
                match futures::future::select(packet_future, stop_future).await {
                    Either::Left((packet_result, _)) => Either::Left(packet_result),
                    Either::Right((stop, _)) => Either::Right(stop),
                }
            };
            let packet_result = match (event, stop_rx.try_recv()) {
                (Either::Right(stop), _) | (Either::Left(_), Ok(stop)) => {
                    self.stop_reading(&stop, writer);
                    return Ok(());
                }
                (Either::Left(packet_result), Err(_)) => packet_result,
            };
            match packet_result {
                Ok(packet) => {
                    last_packet_time.set(mqtt5::time::Instant::now());
                    if let Err(e) = Box::pin(self.handle_packet(packet, writer)).await {
                        error!("Error handling packet: {e}");
                        return Err(e);
                    }
                }
                Err(e) => {
                    debug!("Connection closed: {e}");
                    return Ok(());
                }
            }
        }
    }

    fn stop_reading(&self, stop: &StopReason, writer: &SharedWriter) {
        match stop {
            StopReason::KeepAliveTimeout => info!("Keep-alive timeout, disconnecting"),
            StopReason::TakenOver => {
                info!("Session taken over, disconnecting");
                if self.protocol_version == 5 {
                    let disconnect = mqtt5_protocol::packet::disconnect::DisconnectPacket::new(
                        mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::SessionTakenOver,
                    );
                    if let Err(e) = self.write_packet(&Packet::Disconnect(disconnect), writer) {
                        debug!("Failed to send the session taken over DISCONNECT: {e}");
                    }
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
                self.handle_puback(puback).await;
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
            Packet::Connect(_) => {
                if self.protocol_version == 5 {
                    let disconnect = mqtt5_protocol::packet::disconnect::DisconnectPacket::new(
                        mqtt5_protocol::protocol::v5::reason_codes::ReasonCode::ProtocolError,
                    );
                    self.write_packet(&Packet::Disconnect(disconnect), writer)?;
                }
                Err(MqttError::ProtocolError("Duplicate CONNECT".to_string()))
            }
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

    /// Resumes after the highest identifier in flight, so later deliveries sort after it.
    pub(super) fn advance_packet_id_past_inflight(&self) {
        let following = |pid: u16| if pid == u16::MAX { 1 } else { pid + 1 };
        let outbound = self.outbound_inflight.borrow();
        let mut candidate = outbound
            .highest()
            .map_or(self.next_packet_id.get(), following);
        for _ in 0..u16::MAX {
            if !outbound.contains(candidate) && !self.inflight_publishes.contains_key(&candidate) {
                self.next_packet_id.set(candidate);
                return;
            }
            candidate = following(candidate);
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
