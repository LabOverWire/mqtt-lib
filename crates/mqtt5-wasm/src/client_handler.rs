use crate::decoder::read_packet;
use crate::transport::message_port::MessagePortTransport;
use crate::transport::{WasmReader, WasmWriter};
use mqtt5::broker::auth::{AuthProvider, EnhancedAuthResult, EnhancedAuthStatus};
use mqtt5::broker::config::BrokerConfig;
use mqtt5::broker::resource_monitor::ResourceMonitor;
use mqtt5::broker::router::MessageRouter;
use mqtt5::broker::storage::{ClientSession, DynamicStorage, StorageBackend};
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
use mqtt5_protocol::QoS;
use mqtt5_protocol::Transport;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
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
    config: Arc<BrokerConfig>,
    router: Arc<MessageRouter>,
    auth_provider: Arc<dyn AuthProvider>,
    storage: Arc<DynamicStorage>,
    stats: Arc<BrokerStats>,
    resource_monitor: Arc<ResourceMonitor>,
    session: Option<ClientSession>,
    publish_rx: tokio::sync::mpsc::Receiver<PublishPacket>,
    publish_tx: tokio::sync::mpsc::Sender<PublishPacket>,
    inflight_publishes: HashMap<u16, PublishPacket>,
    normal_disconnect: bool,
    keep_alive: mqtt5::time::Duration,
    auth_method: Option<String>,
    auth_state: AuthState,
    pending_connect: Option<PendingConnect>,
}

static HANDLER_COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

impl WasmClientHandler {
    pub fn new(
        port: MessagePort,
        config: Arc<BrokerConfig>,
        router: Arc<MessageRouter>,
        auth_provider: Arc<dyn AuthProvider>,
        storage: Arc<DynamicStorage>,
        stats: Arc<BrokerStats>,
        resource_monitor: Arc<ResourceMonitor>,
    ) {
        let (publish_tx, publish_rx) = tokio::sync::mpsc::channel(100);
        let handler_id = HANDLER_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        let handler = Self {
            handler_id,
            client_id: None,
            user_id: None,
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
        };

        spawn_local(async move {
            match handler.run(port).await {
                Ok(()) => debug!("Client handler completed normally"),
                Err(e) => error!("Client handler error: {}", e),
            }
        });
    }

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

    async fn wait_for_connect(
        &mut self,
        reader: &mut WasmReader,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        let packet = read_packet(reader).await?;

        match packet {
            Packet::Connect(connect) => self.handle_connect(*connect, writer).await,
            _ => {
                error!("First packet must be CONNECT");
                Err(MqttError::ProtocolError(
                    "First packet must be CONNECT".to_string(),
                ))
            }
        }
    }

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
        let last_packet_time_ka = Rc::clone(&last_packet_time);
        let running_ka = Rc::clone(&running);
        let keep_alive = self.keep_alive;
        let handler_id = self.handler_id;

        if !keep_alive.is_zero() {
            spawn_local(async move {
                let timeout =
                    keep_alive + mqtt5::time::Duration::from_secs(keep_alive.as_secs() / 2);
                loop {
                    gloo_timers::future::sleep(std::time::Duration::from_secs(1)).await;

                    if !*running_ka.borrow() {
                        break;
                    }

                    let elapsed = last_packet_time_ka.get().elapsed();
                    if elapsed > timeout {
                        warn!("Handler #{} keep-alive timeout", handler_id);
                        *running_ka.borrow_mut() = false;
                        break;
                    }
                }
            });
        }

        let running_forward = Rc::clone(&running);
        let mut publish_rx =
            std::mem::replace(&mut self.publish_rx, tokio::sync::mpsc::channel(1).1);
        let writer_shared = Rc::new(RefCell::new(writer));
        let writer_for_forward = Rc::clone(&writer_shared);

        spawn_local(async move {
            loop {
                if !*running_forward.borrow() {
                    break;
                }

                match publish_rx.recv().await {
                    Some(publish) => {
                        if let Ok(mut writer_guard) = writer_for_forward.try_borrow_mut() {
                            let result =
                                Self::write_publish_packet(&publish, &mut *writer_guard).await;
                            if let Err(e) = result {
                                error!("Handler #{} error forwarding publish: {}", handler_id, e);
                                break;
                            }
                        } else {
                            error!("Handler #{} writer busy, cannot forward", handler_id);
                        }
                    }
                    None => break,
                }
            }
        });

        loop {
            if !*running.borrow() {
                info!("Session takeover or keep-alive timeout, disconnecting");
                return Ok(());
            }

            match read_packet(reader).await {
                Ok(packet) => {
                    last_packet_time.set(mqtt5::time::Instant::now());
                    if let Ok(mut writer_guard) = writer_shared.try_borrow_mut() {
                        if let Err(e) = self.handle_packet(packet, &mut *writer_guard).await {
                            error!("Error handling packet: {}", e);
                            return Err(e);
                        }
                    } else {
                        error!("Handler writer busy in main loop");
                    }
                }
                Err(e) => {
                    debug!("Connection closed: {}", e);
                    return Ok(());
                }
            }
        }
    }

    async fn write_publish_packet(publish: &PublishPacket, writer: &mut WasmWriter) -> Result<()> {
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
            Packet::PubAck(puback) => self.handle_puback(puback).await,
            Packet::PubRec(pubrec) => self.handle_pubrec(pubrec, writer).await,
            Packet::PubRel(pubrel) => self.handle_pubrel(pubrel, writer).await,
            Packet::PubComp(pubcomp) => self.handle_pubcomp(pubcomp).await,
            Packet::PingReq => self.handle_pingreq(writer).await,
            Packet::Disconnect(disconnect) => self.handle_disconnect(disconnect).await,
            Packet::Auth(auth) => self.handle_auth(auth, writer).await,
            _ => {
                warn!("Unexpected packet type");
                Ok(())
            }
        }
    }

    async fn handle_connect(
        &mut self,
        mut connect: ConnectPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        if connect.protocol_version != 5 {
            let connack = ConnAckPacket::new(false, ReasonCode::UnsupportedProtocolVersion);
            self.write_packet(Packet::ConnAck(connack), writer).await?;
            return Err(MqttError::ProtocolError(
                "Unsupported protocol version".to_string(),
            ));
        }

        let mut assigned_client_id = None;
        if connect.client_id.is_empty() {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let generated_id = format!("wasm-auto-{}", COUNTER.fetch_add(1, Ordering::SeqCst));
            debug!("Generated client ID '{}' for empty client ID", generated_id);
            connect.client_id.clone_from(&generated_id);
            assigned_client_id = Some(generated_id);
        }

        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        let dummy_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 0);

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

        let auth_result = self
            .auth_provider
            .authenticate(&connect, dummy_addr)
            .await?;

        if !auth_result.authenticated {
            let connack = ConnAckPacket::new(false, auth_result.reason_code);
            self.write_packet(Packet::ConnAck(connack), writer).await?;
            return Err(MqttError::AuthenticationFailed);
        }

        self.client_id = Some(connect.client_id.clone());
        self.user_id = auth_result.user_id;
        self.keep_alive = mqtt5::time::Duration::from_secs(u64::from(connect.keep_alive));
        self.auth_state = AuthState::Completed;

        let session_present = self.handle_session(&connect).await?;

        let mut connack = ConnAckPacket::new(session_present, ReasonCode::Success);

        if let Some(ref assigned_id) = assigned_client_id {
            connack
                .properties
                .set_assigned_client_identifier(assigned_id.clone());
        }

        connack
            .properties
            .set_session_expiry_interval(self.config.session_expiry_interval.as_secs() as u32);
        if self.config.maximum_qos < 2 {
            connack.properties.set_maximum_qos(self.config.maximum_qos);
        }
        connack.properties.set_retain_available(true);
        connack
            .properties
            .set_maximum_packet_size(self.config.max_packet_size as u32);
        connack
            .properties
            .set_topic_alias_maximum(self.config.topic_alias_maximum);
        connack
            .properties
            .set_wildcard_subscription_available(self.config.wildcard_subscription_available);
        connack
            .properties
            .set_subscription_identifier_available(self.config.subscription_identifier_available);
        connack
            .properties
            .set_shared_subscription_available(self.config.shared_subscription_available);

        self.write_packet(Packet::ConnAck(connack), writer).await?;

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
                self.send_publish(publish, writer).await?;
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

        for filter in &subscribe.filters {
            let authorized = self
                .auth_provider
                .authorize_subscribe(&client_id, self.user_id.as_deref(), &filter.filter)
                .await?;

            if !authorized {
                reason_codes.push(SubAckReasonCode::NotAuthorized);
                continue;
            }

            let granted_qos = if filter.options.qos as u8 > self.config.maximum_qos {
                QoS::from(self.config.maximum_qos)
            } else {
                filter.options.qos
            };

            self.router
                .subscribe(
                    client_id.clone(),
                    filter.filter.clone(),
                    granted_qos,
                    None,
                    filter.options.no_local,
                    filter.options.retain_as_published,
                )
                .await;

            if let Some(ref mut session) = self.session {
                session.add_subscription(filter.filter.clone(), granted_qos);
                self.storage.store_session(session.clone()).await.ok();
            }

            if filter.options.retain_handling
                != mqtt5_protocol::packet::subscribe::RetainHandling::DoNotSend
            {
                let retained = self.router.get_retained_messages(&filter.filter).await;
                for mut msg in retained {
                    msg.retain = true;
                    self.send_publish(msg, writer).await?;
                }
            }

            reason_codes.push(SubAckReasonCode::from_qos(granted_qos));
        }

        let mut suback = SubAckPacket::new(subscribe.packet_id);
        suback.reason_codes = reason_codes;

        self.write_packet(Packet::SubAck(suback), writer).await?;

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

        self.write_packet(Packet::UnsubAck(unsuback), writer)
            .await?;

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
                        self.write_packet(Packet::PubAck(puback), writer).await?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(packet_id);
                        pubrec.reason_code = ReasonCode::NotAuthorized;
                        self.write_packet(Packet::PubRec(pubrec), writer).await?;
                    }
                    _ => {}
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
                        self.write_packet(Packet::PubAck(puback), writer).await?;
                    }
                    QoS::ExactlyOnce => {
                        let mut pubrec = PubRecPacket::new(packet_id);
                        pubrec.reason_code = ReasonCode::QuotaExceeded;
                        self.write_packet(Packet::PubRec(pubrec), writer).await?;
                    }
                    _ => {}
                }
            }
            return Ok(());
        }

        match publish.qos {
            QoS::AtMostOnce => {
                self.router.route_message(&publish, Some(client_id)).await;
            }
            QoS::AtLeastOnce => {
                self.router.route_message(&publish, Some(client_id)).await;
                let puback = PubAckPacket::new(publish.packet_id.unwrap());
                self.write_packet(Packet::PubAck(puback), writer).await?;
            }
            QoS::ExactlyOnce => {
                let packet_id = publish.packet_id.unwrap();
                self.inflight_publishes.insert(packet_id, publish);
                let pubrec = PubRecPacket::new(packet_id);
                self.write_packet(Packet::PubRec(pubrec), writer).await?;
            }
        }

        Ok(())
    }

    async fn handle_puback(&mut self, _puback: PubAckPacket) -> Result<()> {
        Ok(())
    }

    async fn handle_pubrec(&mut self, pubrec: PubRecPacket, writer: &mut WasmWriter) -> Result<()> {
        let pubrel = PubRelPacket::new(pubrec.packet_id);
        self.write_packet(Packet::PubRel(pubrel), writer).await?;
        Ok(())
    }

    async fn handle_pubrel(&mut self, pubrel: PubRelPacket, writer: &mut WasmWriter) -> Result<()> {
        if let Some(publish) = self.inflight_publishes.remove(&pubrel.packet_id) {
            let client_id = self.client_id.as_ref().unwrap();
            self.router.route_message(&publish, Some(client_id)).await;
        }

        let pubcomp = PubCompPacket::new(pubrel.packet_id);
        self.write_packet(Packet::PubComp(pubcomp), writer).await?;
        Ok(())
    }

    async fn handle_pubcomp(&mut self, _pubcomp: PubCompPacket) -> Result<()> {
        Ok(())
    }

    async fn handle_pingreq(&mut self, writer: &mut WasmWriter) -> Result<()> {
        self.write_packet(Packet::PingResp, writer).await
    }

    async fn handle_disconnect(&mut self, _disconnect: DisconnectPacket) -> Result<()> {
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
                    self.write_packet(Packet::Disconnect(disconnect), writer)
                        .await?;
                    return Err(MqttError::ProtocolError(
                        "AUTH received outside of auth flow".to_string(),
                    ));
                }

                let auth_method = match self.auth_method.clone() {
                    Some(m) => m,
                    None => {
                        return Err(MqttError::ProtocolError("No auth method set".to_string()));
                    }
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
                    self.write_packet(Packet::Disconnect(disconnect), writer)
                        .await?;
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
                    self.write_packet(Packet::Disconnect(disconnect), writer)
                        .await?;
                    return Err(MqttError::ProtocolError(
                        "Re-auth before initial auth".to_string(),
                    ));
                }

                let auth_method = match auth.properties.get_authentication_method() {
                    Some(m) => m.clone(),
                    None => {
                        let disconnect = DisconnectPacket {
                            reason_code: ReasonCode::ProtocolError,
                            properties:
                                mqtt5_protocol::protocol::v5::properties::Properties::default(),
                        };
                        self.write_packet(Packet::Disconnect(disconnect), writer)
                            .await?;
                        return Err(MqttError::ProtocolError(
                            "Re-auth missing method".to_string(),
                        ));
                    }
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
                        self.write_packet(Packet::Auth(response), writer).await?;
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
                        self.write_packet(Packet::Auth(response), writer).await?;
                        Ok(())
                    }
                    EnhancedAuthStatus::Failed => {
                        warn!("Re-authentication failed for {}", client_id);
                        let disconnect = DisconnectPacket {
                            reason_code: result.reason_code,
                            properties:
                                mqtt5_protocol::protocol::v5::properties::Properties::default(),
                        };
                        self.write_packet(Packet::Disconnect(disconnect), writer)
                            .await?;
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

                    connack.properties.set_session_expiry_interval(
                        self.config.session_expiry_interval.as_secs() as u32,
                    );
                    if self.config.maximum_qos < 2 {
                        connack.properties.set_maximum_qos(self.config.maximum_qos);
                    }
                    connack.properties.set_retain_available(true);
                    connack
                        .properties
                        .set_maximum_packet_size(self.config.max_packet_size as u32);
                    connack
                        .properties
                        .set_topic_alias_maximum(self.config.topic_alias_maximum);
                    connack.properties.set_wildcard_subscription_available(
                        self.config.wildcard_subscription_available,
                    );
                    connack.properties.set_subscription_identifier_available(
                        self.config.subscription_identifier_available,
                    );
                    connack.properties.set_shared_subscription_available(
                        self.config.shared_subscription_available,
                    );

                    self.write_packet(Packet::ConnAck(connack), writer).await?;

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
                self.write_packet(Packet::Auth(auth_packet), writer).await?;
                Ok(())
            }
            EnhancedAuthStatus::Failed => {
                self.auth_state = AuthState::NotStarted;
                self.pending_connect = None;

                let connack = ConnAckPacket::new(false, result.reason_code);
                self.write_packet(Packet::ConnAck(connack), writer).await?;
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

    async fn send_publish(
        &mut self,
        publish: PublishPacket,
        writer: &mut WasmWriter,
    ) -> Result<()> {
        self.write_packet(Packet::Publish(publish), writer).await
    }

    async fn write_packet(&self, packet: Packet, writer: &mut WasmWriter) -> Result<()> {
        use bytes::BytesMut;
        use mqtt5_protocol::packet::MqttPacket;

        let mut buf = BytesMut::new();

        match &packet {
            Packet::ConnAck(p) => p.encode(&mut buf)?,
            Packet::SubAck(p) => p.encode(&mut buf)?,
            Packet::UnsubAck(p) => p.encode(&mut buf)?,
            Packet::Publish(p) => p.encode(&mut buf)?,
            Packet::PubAck(p) => p.encode(&mut buf)?,
            Packet::PubRec(p) => p.encode(&mut buf)?,
            Packet::PubRel(p) => p.encode(&mut buf)?,
            Packet::PubComp(p) => p.encode(&mut buf)?,
            Packet::PingResp => {
                mqtt5_protocol::packet::pingresp::PingRespPacket::default().encode(&mut buf)?
            }
            Packet::Disconnect(p) => p.encode(&mut buf)?,
            Packet::Auth(p) => p.encode(&mut buf)?,
            _ => {
                return Err(MqttError::ProtocolError(format!(
                    "Encoding not yet implemented for packet type: {:?}",
                    packet
                )));
            }
        }

        writer.write(&buf)?;
        Ok(())
    }
}
