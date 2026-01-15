use crate::broker::WasmEventCallbacks;
use crate::decoder::read_packet;
use crate::transport::message_port::MessagePortTransport;
use crate::transport::{WasmReader, WasmWriter};
use mqtt5::broker::auth::{AuthProvider, EnhancedAuthResult, EnhancedAuthStatus};
use mqtt5::broker::config::BrokerConfig;
use mqtt5::broker::resource_monitor::ResourceMonitor;
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::storage::{ClientSession, DynamicStorage, StorageBackend, StoredSubscription};
use mqtt5::broker::sys_topics::BrokerStats;
use mqtt5_protocol::error::{MqttError, Result};
use mqtt5_protocol::packet::auth::AuthPacket;
use mqtt5_protocol::packet::connack::ConnAckPacket;
use mqtt5_protocol::packet::connect::ConnectPacket;
use mqtt5_protocol::packet::disconnect::DisconnectPacket;
use mqtt5_protocol::packet::puback::PubAckPacket;
use mqtt5_protocol::packet::pubcomp::PubCompPacket;
use mqtt5_protocol::packet::publish::PublishPacket;
use mqtt5_protocol::packet::pubrec::PubRecPacket;
use mqtt5_protocol::packet::pubrel::PubRelPacket;
use mqtt5_protocol::packet::suback::{SubAckPacket, SubAckReasonCode};
use mqtt5_protocol::packet::subscribe::SubscribePacket;
use mqtt5_protocol::packet::unsuback::{UnsubAckPacket, UnsubAckReasonCode};
use mqtt5_protocol::packet::unsubscribe::UnsubscribePacket;
use mqtt5_protocol::packet::Packet;
use mqtt5_protocol::protocol::v5::reason_codes::ReasonCode;
use mqtt5_protocol::types::ProtocolVersion;
use mqtt5_protocol::KeepaliveConfig;
use mqtt5_protocol::QoS;
use mqtt5_protocol::Transport;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{debug, error, info, warn};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;
use web_sys::MessagePort;

#[derive(Debug, Clone, PartialEq)]
enum AuthState {
    NotStarted,
    InProgress,
    Completed,
}

struct PendingConnect {
    connect: ConnectPacket,
    assigned_client_id: Option<String>,
}

pub struct WasmClientHandler {
    handler_id: u32,
    client_id: Option<String>,
    user_id: Option<String>,
    protocol_version: u8,
    config: Arc<RwLock<BrokerConfig>>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<dyn AuthProvider>,
    storage: Arc<DynamicStorage>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    session: Option<ClientSession>,
    publish_rx: flume::Receiver<PublishPacket>,
    publish_tx: flume::Sender<PublishPacket>,
    inflight_publishes: HashMap<u16, PublishPacket>,
    normal_disconnect: bool,
    keep_alive: mqtt5::time::Duration,
    auth_method: Option<String>,
    auth_state: AuthState,
    pending_connect: Option<PendingConnect>,
    event_callbacks: WasmEventCallbacks,
}

static HANDLER_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

impl WasmClientHandler {
    #[allow(dead_code)]
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
        use std::cell::RefCell;
        use std::rc::Rc;
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

    #[allow(clippy::must_use_candidate, clippy::new_ret_no_self)]
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
        let writer = MessagePortWriter::new(port, connected);

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

    async fn wait_for_connect(
        &mut self,
        reader: &mut WasmReader,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let packet = read_packet(reader).await?;

        if let Packet::Connect(connect) = packet {
            self.handle_connect(*connect, writer).await
        } else {
            error!("First packet must be CONNECT");
            Err(MqttError::ProtocolError(
                "First packet must be CONNECT".to_string(),
            ))
        }
    }

    #[allow(clippy::await_holding_refcell_ref, clippy::too_many_lines)]
    async fn packet_loop(
        &mut self,
        reader: &mut WasmReader,
        writer: WasmWriter,
        disconnect_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<()> {
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;
        use wasm_bindgen_futures::spawn_local;

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

        spawn_local(async move {
            loop {
                if !*running_forward.borrow() {
                    break;
                }

                match publish_rx.recv_async().await {
                    Ok(publish) => {
                        if let Ok(mut writer_guard) = writer_for_forward.try_borrow_mut() {
                            let result = Self::write_publish_packet(&publish, &mut writer_guard);
                            if let Err(e) = result {
                                error!("Handler #{} error forwarding publish: {}", handler_id, e);
                                break;
                            }
                        } else {
                            error!("Handler #{} writer busy, cannot forward", handler_id);
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

    fn write_publish_packet(publish: &PublishPacket, writer: &mut WasmWriter) -> Result<()> {
        use bytes::BytesMut;
        use mqtt5_protocol::packet::MqttPacket;

        let mut buf = BytesMut::new();
        publish.encode(&mut buf)?;
        writer.write(&buf)?;
        Ok(())
    }

    async fn handle_packet(&mut self, packet: Packet, writer: &mut WasmWriter) -> Result<()> {
        match packet {
            Packet::Subscribe(subscribe) => self.handle_subscribe(subscribe, writer).await,
            Packet::Unsubscribe(unsubscribe) => self.handle_unsubscribe(unsubscribe, writer).await,
            Packet::Publish(publish) => self.handle_publish(publish, writer).await,
            Packet::PubAck(puback) => self.handle_puback(puback),
            Packet::PubRec(pubrec) => self.handle_pubrec(&pubrec, writer),
            Packet::PubRel(pubrel) => self.handle_pubrel(pubrel, writer).await,
            Packet::PubComp(pubcomp) => self.handle_pubcomp(pubcomp),
            Packet::PingReq => self.handle_pingreq(writer),
            Packet::Disconnect(disconnect) => self.handle_disconnect(disconnect),
            Packet::Auth(auth) => self.handle_auth(auth, writer).await,
            _ => {
                warn!("Unexpected packet type");
                Ok(())
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    async fn handle_connect(
        &mut self,
        mut connect: ConnectPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};

        if connect.protocol_version != 4 && connect.protocol_version != 5 {
            let mut connack = ConnAckPacket::new(false, ReasonCode::UnsupportedProtocolVersion);
            connack.protocol_version = connect.protocol_version;
            self.write_packet(&Packet::ConnAck(connack), writer)?;
            return Err(MqttError::ProtocolError(
                "Unsupported protocol version".to_string(),
            ));
        }
        self.protocol_version = connect.protocol_version;

        let mut assigned_client_id = None;
        if connect.client_id.is_empty() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let generated_id = format!("wasm-auto-{}", COUNTER.fetch_add(1, Ordering::SeqCst));
            debug!("Generated client ID '{}' for empty client ID", generated_id);
            connect.client_id.clone_from(&generated_id);
            assigned_client_id = Some(generated_id);
        }

        let dummy_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

        if self.protocol_version == 5 {
            if let Some(auth_method) = connect.properties.get_authentication_method() {
                if self.auth_provider.supports_enhanced_auth() {
                    self.auth_method = Some(auth_method.clone());
                    self.auth_state = AuthState::InProgress;
                    self.pending_connect = Some(PendingConnect {
                        connect: connect.clone(),
                        assigned_client_id: assigned_client_id.clone(),
                    });

                    let auth_data = connect.properties.get_authentication_data();
                    let result = self
                        .auth_provider
                        .authenticate_enhanced(auth_method, auth_data, &connect.client_id)
                        .await?;

                    return self.process_enhanced_auth_result(result, writer).await;
                }
            }
        }

        let auth_result = self
            .auth_provider
            .authenticate(&connect, dummy_addr)
            .await?;

        if !auth_result.authenticated {
            let mut connack = ConnAckPacket::new(false, auth_result.reason_code);
            connack.protocol_version = self.protocol_version;
            self.write_packet(&Packet::ConnAck(connack), writer)?;
            return Err(MqttError::AuthenticationFailed);
        }

        self.client_id = Some(connect.client_id.clone());
        self.user_id = auth_result.user_id;
        self.keep_alive = mqtt5::time::Duration::from_secs(u64::from(connect.keep_alive));
        self.auth_state = AuthState::Completed;

        let session_present = self.handle_session(&connect).await?;

        let mut connack = ConnAckPacket::new(session_present, ReasonCode::Success);
        connack.protocol_version = self.protocol_version;

        if self.protocol_version == 5 {
            if let Some(ref assigned_id) = assigned_client_id {
                connack
                    .properties
                    .set_assigned_client_identifier(assigned_id.clone());
            }

            if let Ok(config) = self.config.read() {
                connack
                    .properties
                    .set_session_expiry_interval(config.session_expiry_interval.as_secs() as u32);
                if config.maximum_qos < 2 {
                    connack.properties.set_maximum_qos(config.maximum_qos);
                }
                connack.properties.set_retain_available(true);
                connack
                    .properties
                    .set_maximum_packet_size(config.max_packet_size as u32);
                connack
                    .properties
                    .set_topic_alias_maximum(config.topic_alias_maximum);
                connack
                    .properties
                    .set_wildcard_subscription_available(config.wildcard_subscription_available);
                connack.properties.set_subscription_identifier_available(
                    config.subscription_identifier_available,
                );
                connack
                    .properties
                    .set_shared_subscription_available(config.shared_subscription_available);
            }
        }

        self.write_packet(&Packet::ConnAck(connack), writer)?;

        self.fire_client_connect(&connect.client_id, connect.clean_start);

        if session_present {
            self.deliver_queued_messages(&connect.client_id, writer)
                .await?;
        }
        Ok(())
    }

    async fn handle_session(&mut self, connect: &ConnectPacket) -> Result<bool> {
        let client_id = &connect.client_id;
        let session_expiry = connect.properties.get_session_expiry_interval();

        if connect.clean_start {
            self.storage.remove_session(client_id).await.ok();
            self.storage.remove_queued_messages(client_id).await.ok();

            let session = ClientSession::new_with_will(
                client_id.clone(),
                session_expiry != Some(0),
                session_expiry,
                connect.will.clone(),
            );
            self.storage.store_session(session.clone()).await.ok();
            self.session = Some(session);
            Ok(false)
        } else {
            match self.storage.get_session(client_id).await {
                Ok(Some(mut session)) => {
                    for (topic_filter, stored) in &session.subscriptions {
                        self.router
                            .subscribe(
                                client_id.clone(),
                                topic_filter.clone(),
                                stored.qos,
                                stored.subscription_id,
                                stored.no_local,
                                stored.retain_as_published,
                                stored.retain_handling,
                                ProtocolVersion::try_from(self.protocol_version)
                                    .unwrap_or_default(),
                            )
                            .await?;
                    }

                    session.will_message.clone_from(&connect.will);
                    session.will_delay_interval = connect
                        .will
                        .as_ref()
                        .and_then(|w| w.properties.will_delay_interval);
                    self.storage.store_session(session.clone()).await.ok();
                    self.session = Some(session);
                    Ok(true)
                }
                Ok(None) => {
                    let session = ClientSession::new_with_will(
                        client_id.clone(),
                        session_expiry != Some(0),
                        session_expiry,
                        connect.will.clone(),
                    );
                    self.storage.store_session(session.clone()).await.ok();
                    self.session = Some(session);
                    Ok(false)
                }
                Err(e) => Err(e),
            }
        }
    }

    async fn deliver_queued_messages(
        &mut self,
        client_id: &str,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let queued_messages = self.storage.get_queued_messages(client_id).await?;
        self.storage.remove_queued_messages(client_id).await?;

        if !queued_messages.is_empty() {
            debug!(
                "Delivering {} queued messages to {}",
                queued_messages.len(),
                client_id
            );
            for msg in queued_messages {
                let publish = msg.to_publish_packet();
                self.send_publish(publish, writer)?;
            }
        }
        Ok(())
    }

    async fn handle_subscribe(
        &mut self,
        subscribe: SubscribePacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let client_id = self.client_id.clone().unwrap();
        let mut reason_codes = Vec::new();
        let mut successful_subscriptions = Vec::new();

        for filter in &subscribe.filters {
            let authorized = self
                .auth_provider
                .authorize_subscribe(&client_id, self.user_id.as_deref(), &filter.filter)
                .await?;

            if !authorized {
                reason_codes.push(SubAckReasonCode::NotAuthorized);
                continue;
            }

            let max_qos = self
                .config
                .read()
                .map(|c| c.maximum_qos)
                .unwrap_or_else(|_| {
                    warn!("Config read failed for max_qos, using default 2");
                    2
                });
            let granted_qos = if filter.options.qos as u8 > max_qos {
                QoS::from(max_qos)
            } else {
                filter.options.qos
            };

            let subscription_id = subscribe.properties.get_subscription_identifier();

            self.router
                .subscribe(
                    client_id.clone(),
                    filter.filter.clone(),
                    granted_qos,
                    subscription_id,
                    filter.options.no_local,
                    filter.options.retain_as_published,
                    filter.options.retain_handling as u8,
                    ProtocolVersion::try_from(self.protocol_version).unwrap_or_default(),
                )
                .await?;

            if let Some(ref mut session) = self.session {
                let stored = StoredSubscription {
                    qos: granted_qos,
                    no_local: filter.options.no_local,
                    retain_as_published: filter.options.retain_as_published,
                    retain_handling: filter.options.retain_handling as u8,
                    subscription_id,
                    protocol_version: self.protocol_version,
                };
                session.add_subscription(filter.filter.clone(), stored);
                self.storage.store_session(session.clone()).await.ok();
            }

            if filter.options.retain_handling
                != mqtt5_protocol::packet::subscribe::RetainHandling::DoNotSend
            {
                let retained = self.router.get_retained_messages(&filter.filter).await;
                for mut msg in retained {
                    msg.retain = true;
                    self.send_publish(msg, writer)?;
                }
            }

            successful_subscriptions.push((filter.filter.clone(), granted_qos as u8));
            reason_codes.push(SubAckReasonCode::from_qos(granted_qos));
        }

        let mut suback = SubAckPacket::new(subscribe.packet_id);
        suback.reason_codes = reason_codes;
        suback.protocol_version = self.protocol_version;

        self.write_packet(&Packet::SubAck(suback), writer)?;

        if !successful_subscriptions.is_empty() {
            self.fire_client_subscribe(&client_id, &successful_subscriptions);
        }

        debug!("Client {} subscribed to topics", client_id);
        Ok(())
    }

    async fn handle_unsubscribe(
        &mut self,
        unsubscribe: UnsubscribePacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        let mut reason_codes = Vec::new();

        for filter in &unsubscribe.filters {
            self.router.unsubscribe(client_id, filter).await;

            if let Some(ref mut session) = self.session {
                session.remove_subscription(filter);
                self.storage.store_session(session.clone()).await.ok();
            }

            reason_codes.push(UnsubAckReasonCode::Success);
        }

        let mut unsuback = UnsubAckPacket::new(unsubscribe.packet_id);
        unsuback.reason_codes = reason_codes;
        unsuback.protocol_version = self.protocol_version;

        self.write_packet(&Packet::UnsubAck(unsuback), writer)?;

        if !unsubscribe.filters.is_empty() {
            self.fire_client_unsubscribe(client_id, &unsubscribe.filters);
        }

        Ok(())
    }

    async fn handle_publish(
        &mut self,
        publish: PublishPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let client_id = self.client_id.as_ref().unwrap();
        self.stats.publish_received(publish.payload.len());

        let authorized = self
            .auth_provider
            .authorize_publish(client_id, self.user_id.as_deref(), &publish.topic_name)
            .await?;

        if !authorized {
            warn!(
                "Client {} not authorized to publish to {}",
                client_id, publish.topic_name
            );
            if let Some(packet_id) = publish.packet_id {
                match publish.qos {
                    QoS::AtLeastOnce => {
                        let mut puback = PubAckPacket::new(packet_id);
                        puback.reason_code = ReasonCode::NotAuthorized;
                        self.write_packet(&Packet::PubAck(puback), writer)?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(packet_id);
                        pubrec.reason_code = ReasonCode::NotAuthorized;
                        self.write_packet(&Packet::PubRec(pubrec), writer)?;
                    }
                    QoS::AtMostOnce => {}
                }
            }
            return Ok(());
        }

        let max_qos = self
            .config
            .read()
            .map(|c| c.maximum_qos)
            .unwrap_or_else(|_| {
                warn!("Config read failed for max_qos, using default 2");
                2
            });
        if (publish.qos as u8) > max_qos {
            debug!(
                "Client {} sent QoS {} but max is {}",
                client_id, publish.qos as u8, max_qos
            );
            if let Some(packet_id) = publish.packet_id {
                match publish.qos {
                    QoS::AtLeastOnce => {
                        let mut puback = PubAckPacket::new(packet_id);
                        puback.reason_code = ReasonCode::QoSNotSupported;
                        self.write_packet(&Packet::PubAck(puback), writer)?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(packet_id);
                        pubrec.reason_code = ReasonCode::QoSNotSupported;
                        self.write_packet(&Packet::PubRec(pubrec), writer)?;
                    }
                    QoS::AtMostOnce => {}
                }
            }
            return Ok(());
        }

        let payload_size = publish.payload.len();
        if !self
            .resource_monitor
            .can_send_message(client_id, payload_size)
            .await
        {
            warn!("Client {} exceeded quota", client_id);
            if let Some(packet_id) = publish.packet_id {
                match publish.qos {
                    QoS::AtLeastOnce => {
                        let mut puback = PubAckPacket::new(packet_id);
                        puback.reason_code = ReasonCode::QuotaExceeded;
                        self.write_packet(&Packet::PubAck(puback), writer)?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(packet_id);
                        pubrec.reason_code = ReasonCode::QuotaExceeded;
                        self.write_packet(&Packet::PubRec(pubrec), writer)?;
                    }
                    QoS::AtMostOnce => {}
                }
            }
            return Ok(());
        }

        match publish.qos {
            QoS::AtMostOnce => {
                self.fire_client_publish(
                    client_id,
                    &publish.topic_name,
                    publish.qos as u8,
                    publish.retain,
                    payload_size,
                );
                self.router.route_message(&publish, Some(client_id)).await;
            }
            QoS::AtLeastOnce => {
                self.fire_client_publish(
                    client_id,
                    &publish.topic_name,
                    publish.qos as u8,
                    publish.retain,
                    payload_size,
                );
                self.router.route_message(&publish, Some(client_id)).await;
                let puback = PubAckPacket::new(publish.packet_id.unwrap());
                self.write_packet(&Packet::PubAck(puback), writer)?;
            }
            QoS::ExactlyOnce => {
                self.fire_client_publish(
                    client_id,
                    &publish.topic_name,
                    publish.qos as u8,
                    publish.retain,
                    payload_size,
                );
                let packet_id = publish.packet_id.unwrap();
                self.inflight_publishes.insert(packet_id, publish);
                let pubrec = PubRecPacket::new(packet_id);
                self.write_packet(&Packet::PubRec(pubrec), writer)?;
            }
        }

        Ok(())
    }

    fn handle_puback(&self, puback: PubAckPacket) -> Result<()> {
        if let Some(client_id) = self.client_id.as_ref() {
            self.fire_message_delivered(client_id, puback.packet_id, 1);
        }
        Ok(())
    }

    #[allow(clippy::similar_names)]
    fn handle_pubrec(&self, pubrec: &PubRecPacket, writer: &mut WasmWriter) -> Result<()> {
        let pubrel = PubRelPacket::new(pubrec.packet_id);
        self.write_packet(&Packet::PubRel(pubrel), writer)?;
        Ok(())
    }

    async fn handle_pubrel(&mut self, pubrel: PubRelPacket, writer: &mut WasmWriter) -> Result<()> {
        if let Some(publish) = self.inflight_publishes.remove(&pubrel.packet_id) {
            let client_id = self.client_id.as_ref().unwrap();
            self.router.route_message(&publish, Some(client_id)).await;
        }

        let pubcomp = PubCompPacket::new(pubrel.packet_id);
        self.write_packet(&Packet::PubComp(pubcomp), writer)?;
        Ok(())
    }

    fn handle_pubcomp(&self, pubcomp: PubCompPacket) -> Result<()> {
        if let Some(client_id) = self.client_id.as_ref() {
            self.fire_message_delivered(client_id, pubcomp.packet_id, 2);
        }
        Ok(())
    }

    fn handle_pingreq(&self, writer: &mut WasmWriter) -> Result<()> {
        self.write_packet(&Packet::PingResp, writer)
    }

    fn handle_disconnect(&mut self, _disconnect: DisconnectPacket) -> Result<()> {
        debug!("Client disconnected normally");
        self.normal_disconnect = true;

        if let Some(ref mut session) = self.session {
            session.will_message = None;
            session.will_delay_interval = None;
        }

        Err(MqttError::ClientClosed)
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_auth(&mut self, auth: AuthPacket, writer: &mut WasmWriter) -> Result<()> {
        let reason_code = auth.reason_code;

        match reason_code {
            ReasonCode::ContinueAuthentication => {
                if self.auth_state != AuthState::InProgress {
                    warn!("AUTH received but not in auth flow");
                    let disconnect = DisconnectPacket {
                        reason_code: ReasonCode::ProtocolError,
                        properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
                    };
                    self.write_packet(&Packet::Disconnect(disconnect), writer)?;
                    return Err(MqttError::ProtocolError(
                        "AUTH received outside of auth flow".to_string(),
                    ));
                }

                let Some(auth_method) = self.auth_method.clone() else {
                    return Err(MqttError::ProtocolError("No auth method set".to_string()));
                };

                let packet_method = auth
                    .properties
                    .get_authentication_method()
                    .cloned()
                    .unwrap_or_default();
                if packet_method != auth_method {
                    warn!(
                        "AUTH method mismatch: expected {}, got {}",
                        auth_method, packet_method
                    );
                    let disconnect = DisconnectPacket {
                        reason_code: ReasonCode::ProtocolError,
                        properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
                    };
                    self.write_packet(&Packet::Disconnect(disconnect), writer)?;
                    return Err(MqttError::ProtocolError("AUTH method mismatch".to_string()));
                }

                let auth_data = auth.properties.get_authentication_data();
                let client_id = self
                    .pending_connect
                    .as_ref()
                    .map(|pc| pc.connect.client_id.clone())
                    .unwrap_or_default();

                let result = self
                    .auth_provider
                    .authenticate_enhanced(&auth_method, auth_data, &client_id)
                    .await?;

                self.process_enhanced_auth_result(result, writer).await
            }
            ReasonCode::ReAuthenticate => {
                if self.auth_state != AuthState::Completed {
                    warn!("Re-auth requested but initial auth not complete");
                    let disconnect = DisconnectPacket {
                        reason_code: ReasonCode::ProtocolError,
                        properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
                    };
                    self.write_packet(&Packet::Disconnect(disconnect), writer)?;
                    return Err(MqttError::ProtocolError(
                        "Re-auth before initial auth".to_string(),
                    ));
                }

                let Some(auth_method) = auth.properties.get_authentication_method().cloned() else {
                    let disconnect = DisconnectPacket {
                        reason_code: ReasonCode::ProtocolError,
                        properties: mqtt5_protocol::protocol::v5::properties::Properties::default(),
                    };
                    self.write_packet(&Packet::Disconnect(disconnect), writer)?;
                    return Err(MqttError::ProtocolError(
                        "Re-auth missing method".to_string(),
                    ));
                };

                let auth_data = auth.properties.get_authentication_data();
                let client_id = self.client_id.clone().unwrap_or_default();

                let result = self
                    .auth_provider
                    .reauthenticate(&auth_method, auth_data, &client_id, self.user_id.as_deref())
                    .await?;

                match result.status {
                    EnhancedAuthStatus::Success => {
                        debug!("Re-authentication successful for {}", client_id);
                        let mut response = AuthPacket::new(ReasonCode::Success);
                        response
                            .properties
                            .set_authentication_method(result.auth_method);
                        if let Some(data) = result.auth_data {
                            response.properties.set_authentication_data(data.into());
                        }
                        self.write_packet(&Packet::Auth(response), writer)?;
                        Ok(())
                    }
                    EnhancedAuthStatus::Continue => {
                        let mut response = AuthPacket::new(ReasonCode::ContinueAuthentication);
                        response
                            .properties
                            .set_authentication_method(result.auth_method);
                        if let Some(data) = result.auth_data {
                            response.properties.set_authentication_data(data.into());
                        }
                        self.write_packet(&Packet::Auth(response), writer)?;
                        Ok(())
                    }
                    EnhancedAuthStatus::Failed => {
                        warn!("Re-authentication failed for {}", client_id);
                        let disconnect = DisconnectPacket {
                            reason_code: result.reason_code,
                            properties:
                                mqtt5_protocol::protocol::v5::properties::Properties::default(),
                        };
                        self.write_packet(&Packet::Disconnect(disconnect), writer)?;
                        Err(MqttError::AuthenticationFailed)
                    }
                }
            }
            _ => {
                warn!("Unexpected AUTH reason code: {:?}", reason_code);
                Ok(())
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    async fn process_enhanced_auth_result(
        &mut self,
        result: EnhancedAuthResult,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        match result.status {
            EnhancedAuthStatus::Success => {
                self.auth_state = AuthState::Completed;

                if let Some(pending) = self.pending_connect.take() {
                    self.client_id = Some(pending.connect.client_id.clone());
                    self.keep_alive =
                        mqtt5::time::Duration::from_secs(u64::from(pending.connect.keep_alive));

                    let session_present = self.handle_session(&pending.connect).await?;

                    let mut connack = ConnAckPacket::new(session_present, ReasonCode::Success);
                    connack.protocol_version = self.protocol_version;
                    if let Some(ref assigned_id) = pending.assigned_client_id {
                        connack
                            .properties
                            .set_assigned_client_identifier(assigned_id.clone());
                    }

                    connack
                        .properties
                        .set_authentication_method(result.auth_method);
                    if let Some(data) = result.auth_data {
                        connack.properties.set_authentication_data(data.into());
                    }

                    if let Ok(config) = self.config.read() {
                        connack.properties.set_session_expiry_interval(
                            config.session_expiry_interval.as_secs() as u32,
                        );
                        if config.maximum_qos < 2 {
                            connack.properties.set_maximum_qos(config.maximum_qos);
                        }
                        connack.properties.set_retain_available(true);
                        connack
                            .properties
                            .set_maximum_packet_size(config.max_packet_size as u32);
                        connack
                            .properties
                            .set_topic_alias_maximum(config.topic_alias_maximum);
                        connack.properties.set_wildcard_subscription_available(
                            config.wildcard_subscription_available,
                        );
                        connack.properties.set_subscription_identifier_available(
                            config.subscription_identifier_available,
                        );
                        connack.properties.set_shared_subscription_available(
                            config.shared_subscription_available,
                        );
                    }

                    self.write_packet(&Packet::ConnAck(connack), writer)?;

                    self.fire_client_connect(
                        &pending.connect.client_id,
                        pending.connect.clean_start,
                    );

                    if session_present {
                        self.deliver_queued_messages(&pending.connect.client_id, writer)
                            .await?;
                    }
                }

                Ok(())
            }
            EnhancedAuthStatus::Continue => {
                let mut auth_packet = AuthPacket::new(ReasonCode::ContinueAuthentication);
                auth_packet
                    .properties
                    .set_authentication_method(result.auth_method);
                if let Some(data) = result.auth_data {
                    auth_packet.properties.set_authentication_data(data.into());
                }
                self.write_packet(&Packet::Auth(auth_packet), writer)?;
                Ok(())
            }
            EnhancedAuthStatus::Failed => {
                self.auth_state = AuthState::NotStarted;
                self.pending_connect = None;

                let mut connack = ConnAckPacket::new(false, result.reason_code);
                connack.protocol_version = self.protocol_version;
                self.write_packet(&Packet::ConnAck(connack), writer)?;
                Err(MqttError::AuthenticationFailed)
            }
        }
    }

    async fn publish_will_message(&self, client_id: &str) {
        if let Some(ref session) = self.session {
            if let Some(ref will) = session.will_message {
                debug!("Publishing will message for client {}", client_id);

                let mut publish =
                    PublishPacket::new(will.topic.clone(), will.payload.clone(), will.qos);
                publish.retain = will.retain;

                if let Some(delay) = session.will_delay_interval {
                    if delay > 0 {
                        debug!("Spawning task to publish will after {} seconds", delay);
                        let router = Arc::clone(&self.router);
                        let publish_clone = publish.clone();
                        let client_id_clone = client_id.to_string();
                        spawn_local(async move {
                            gloo_timers::future::sleep(std::time::Duration::from_secs(u64::from(
                                delay,
                            )))
                            .await;
                            debug!("Publishing delayed will message for {}", client_id_clone);
                            router.route_message(&publish_clone, None).await;
                        });
                    } else {
                        self.router.route_message(&publish, None).await;
                    }
                } else {
                    self.router.route_message(&publish, None).await;
                }
            }
        }
    }

    fn send_publish(&self, publish: PublishPacket, writer: &mut WasmWriter) -> Result<()> {
        self.write_packet(&Packet::Publish(publish), writer)
    }

    #[allow(clippy::unused_self)]
    fn write_packet(&self, packet: &Packet, writer: &mut WasmWriter) -> Result<()> {
        use bytes::BytesMut;
        use mqtt5_protocol::packet::MqttPacket;

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

    fn fire_client_connect(&self, client_id: &str, clean_start: bool) {
        if let Some(callback) = self.event_callbacks.on_client_connect.borrow().as_ref() {
            let obj = js_sys::Object::new();
            js_sys::Reflect::set(&obj, &"clientId".into(), &client_id.into()).ok();
            js_sys::Reflect::set(&obj, &"cleanStart".into(), &clean_start.into()).ok();
            let _ = callback.call1(&JsValue::NULL, &obj);
        }
    }

    fn fire_client_disconnect(&self, client_id: &str, reason: &str, unexpected: bool) {
        if let Some(callback) = self.event_callbacks.on_client_disconnect.borrow().as_ref() {
            let obj = js_sys::Object::new();
            js_sys::Reflect::set(&obj, &"clientId".into(), &client_id.into()).ok();
            js_sys::Reflect::set(&obj, &"reason".into(), &reason.into()).ok();
            js_sys::Reflect::set(&obj, &"unexpected".into(), &unexpected.into()).ok();
            let _ = callback.call1(&JsValue::NULL, &obj);
        }
    }

    fn fire_client_publish(
        &self,
        client_id: &str,
        topic: &str,
        qos: u8,
        retain: bool,
        payload_size: usize,
    ) {
        if let Some(callback) = self.event_callbacks.on_client_publish.borrow().as_ref() {
            let obj = js_sys::Object::new();
            js_sys::Reflect::set(&obj, &"clientId".into(), &client_id.into()).ok();
            js_sys::Reflect::set(&obj, &"topic".into(), &topic.into()).ok();
            js_sys::Reflect::set(&obj, &"qos".into(), &JsValue::from_f64(f64::from(qos))).ok();
            js_sys::Reflect::set(&obj, &"retain".into(), &retain.into()).ok();
            js_sys::Reflect::set(
                &obj,
                &"payloadSize".into(),
                &JsValue::from_f64(payload_size as f64),
            )
            .ok();
            let _ = callback.call1(&JsValue::NULL, &obj);
        }
    }

    fn fire_client_subscribe(&self, client_id: &str, subscriptions: &[(String, u8)]) {
        if let Some(callback) = self.event_callbacks.on_client_subscribe.borrow().as_ref() {
            let obj = js_sys::Object::new();
            js_sys::Reflect::set(&obj, &"clientId".into(), &client_id.into()).ok();

            let subs_array = js_sys::Array::new();
            for (topic, qos) in subscriptions {
                let sub_obj = js_sys::Object::new();
                js_sys::Reflect::set(&sub_obj, &"topic".into(), &topic.into()).ok();
                js_sys::Reflect::set(&sub_obj, &"qos".into(), &JsValue::from_f64(f64::from(*qos)))
                    .ok();
                subs_array.push(&sub_obj);
            }
            js_sys::Reflect::set(&obj, &"subscriptions".into(), &subs_array).ok();
            let _ = callback.call1(&JsValue::NULL, &obj);
        }
    }

    fn fire_client_unsubscribe(&self, client_id: &str, topics: &[String]) {
        if let Some(callback) = self.event_callbacks.on_client_unsubscribe.borrow().as_ref() {
            let obj = js_sys::Object::new();
            js_sys::Reflect::set(&obj, &"clientId".into(), &client_id.into()).ok();

            let topics_array = js_sys::Array::new();
            for topic in topics {
                topics_array.push(&JsValue::from_str(topic));
            }
            js_sys::Reflect::set(&obj, &"topics".into(), &topics_array).ok();
            let _ = callback.call1(&JsValue::NULL, &obj);
        }
    }

    fn fire_message_delivered(&self, client_id: &str, packet_id: u16, qos: u8) {
        if let Some(callback) = self.event_callbacks.on_message_delivered.borrow().as_ref() {
            let obj = js_sys::Object::new();
            js_sys::Reflect::set(&obj, &"clientId".into(), &client_id.into()).ok();
            js_sys::Reflect::set(
                &obj,
                &"packetId".into(),
                &JsValue::from_f64(f64::from(packet_id)),
            )
            .ok();
            js_sys::Reflect::set(&obj, &"qos".into(), &JsValue::from_f64(f64::from(qos))).ok();
            let _ = callback.call1(&JsValue::NULL, &obj);
        }
    }
}
