use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

pub type ClientFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

#[derive(Debug, Clone)]
pub struct WillSpec {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: u8,
}

#[derive(Debug, Clone)]
pub struct AuthSpec {
    pub method: String,
    pub initial_response: Option<Vec<u8>>,
    pub challenge_response: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ConnectSpec {
    pub client_id: String,
    pub keep_alive: Duration,
    pub clean_start: bool,
    pub session_expiry_interval: Option<u32>,
    pub reconnect_delay: Option<(Duration, Duration)>,
    pub will: Option<WillSpec>,
    pub credentials: Option<(String, Vec<u8>)>,
    pub authentication: Option<AuthSpec>,
    pub request_problem_information: Option<bool>,
    pub receive_maximum: Option<u16>,
    pub topic_alias_maximum: Option<u16>,
    pub maximum_packet_size: Option<u32>,
    pub deferred_ack: bool,
}

impl ConnectSpec {
    #[must_use]
    pub fn new(client_id: &str) -> Self {
        Self {
            client_id: client_id.to_owned(),
            keep_alive: Duration::from_secs(60),
            clean_start: true,
            session_expiry_interval: None,
            reconnect_delay: None,
            will: None,
            credentials: None,
            authentication: None,
            request_problem_information: None,
            receive_maximum: None,
            topic_alias_maximum: None,
            maximum_packet_size: None,
            deferred_ack: false,
        }
    }

    #[must_use]
    pub fn with_keep_alive(mut self, keep_alive: Duration) -> Self {
        self.keep_alive = keep_alive;
        self
    }

    #[must_use]
    pub fn resuming(mut self, session_expiry_interval: u32) -> Self {
        self.clean_start = false;
        self.session_expiry_interval = Some(session_expiry_interval);
        self
    }

    #[must_use]
    pub fn reconnecting(self) -> Self {
        self.reconnecting_after(Duration::from_millis(100), Duration::from_millis(300))
    }

    #[must_use]
    pub fn reconnecting_after(mut self, initial: Duration, max: Duration) -> Self {
        self.reconnect_delay = Some((initial, max));
        self
    }

    #[must_use]
    pub fn with_will(mut self, will: WillSpec) -> Self {
        self.will = Some(will);
        self
    }

    #[must_use]
    pub fn with_credentials(mut self, username: &str, password: &[u8]) -> Self {
        self.credentials = Some((username.to_owned(), password.to_vec()));
        self
    }

    #[must_use]
    pub fn with_authentication(mut self, authentication: AuthSpec) -> Self {
        self.authentication = Some(authentication);
        self
    }

    #[must_use]
    pub fn without_problem_information(mut self) -> Self {
        self.request_problem_information = Some(false);
        self
    }

    #[must_use]
    pub fn with_receive_maximum(mut self, receive_maximum: u16) -> Self {
        self.receive_maximum = Some(receive_maximum);
        self
    }

    #[must_use]
    pub fn with_topic_alias_maximum(mut self, topic_alias_maximum: u16) -> Self {
        self.topic_alias_maximum = Some(topic_alias_maximum);
        self
    }

    #[must_use]
    pub fn with_maximum_packet_size(mut self, maximum_packet_size: u32) -> Self {
        self.maximum_packet_size = Some(maximum_packet_size);
        self
    }

    #[must_use]
    pub fn with_deferred_ack(mut self) -> Self {
        self.deferred_ack = true;
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct PublishSpec {
    pub qos: u8,
    pub retain: bool,
    pub topic_alias: Option<u16>,
    pub response_topic: Option<String>,
    pub content_type: Option<String>,
    pub subscription_identifiers: Vec<u32>,
}

impl PublishSpec {
    #[must_use]
    pub fn qos(qos: u8) -> Self {
        Self {
            qos,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ConnectOutcome {
    pub session_present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    Completed,
    Pending,
}

#[derive(Debug, Clone)]
pub struct ClientError(pub String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settle {
    Ack,
    Reject(u8),
}

#[derive(Debug, Clone)]
pub struct ReceivedMessage {
    pub topic: String,
    pub payload: Vec<u8>,
}

pub trait ClientUnderTest: Send + Sync {
    fn connect(&self, url: &str) -> ClientFuture<Result<ConnectOutcome, ClientError>>;
    fn publish(
        &self,
        topic: &str,
        payload: &[u8],
        spec: PublishSpec,
    ) -> ClientFuture<Result<PublishOutcome, ClientError>>;
    fn subscribe(&self, filter: &str, qos: u8) -> ClientFuture<Result<(), ClientError>>;
    fn subscribe_deferred(&self, filter: &str, qos: u8) -> ClientFuture<Result<(), ClientError>>;
    fn settle_deferred(&self, settle: Settle) -> usize;
    fn unsubscribe(&self, filter: &str) -> ClientFuture<Result<(), ClientError>>;
    fn disconnect(&self) -> ClientFuture<Result<(), ClientError>>;
    fn reauthenticate(&self) -> ClientFuture<Result<(), ClientError>>;
    fn is_connected(&self) -> ClientFuture<bool>;
    fn received(&self) -> Vec<ReceivedMessage>;
}

#[derive(Debug, Clone, Copy)]
pub struct ClientCapabilities {
    pub supported: &'static [ClientRequirement],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientRequirement {
    AutomaticReconnect,
    OfflineQueue,
    EnhancedAuth,
    DeferredAck,
}

impl ClientRequirement {
    #[must_use]
    pub fn matches(self, capabilities: &ClientCapabilities) -> bool {
        capabilities.supported.contains(&self)
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AutomaticReconnect => "client.automatic_reconnect",
            Self::OfflineQueue => "client.offline_queue",
            Self::EnhancedAuth => "client.enhanced_auth",
            Self::DeferredAck => "client.deferred_ack",
        }
    }
}

pub trait ClientSut: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> ClientCapabilities;
    fn create(&self, spec: ConnectSpec) -> Arc<dyn ClientUnderTest>;
}

pub type ClientSutHandle = Arc<dyn ClientSut>;

#[cfg(feature = "inprocess-fixture")]
pub use inprocess::inprocess_client;

#[cfg(feature = "inprocess-fixture")]
mod inprocess {
    use super::{
        AuthSpec, ClientCapabilities, ClientError, ClientFuture, ClientRequirement, ClientSut,
        ClientSutHandle, ClientUnderTest, ConnectOutcome, ConnectSpec, PublishOutcome, PublishSpec,
        ReceivedMessage, Settle,
    };
    use mqtt5::protocol::v5::reason_codes::ReasonCode;
    use mqtt5::{
        AckToken, AuthHandler, AuthResponse, ConnectOptions, MqttClient, PublishOptions,
        PublishProperties, PublishResult, QoS, SubscribeOptions, WillMessage,
    };
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex, PoisonError};

    type AuthFuture<'a, T> = Pin<Box<dyn Future<Output = mqtt5::Result<T>> + Send + 'a>>;

    struct ScriptedAuthHandler(AuthSpec);

    impl AuthHandler for ScriptedAuthHandler {
        fn initial_response<'a>(&'a self, auth_method: &'a str) -> AuthFuture<'a, Option<Vec<u8>>> {
            tracing::debug!(auth_method, "scripted authentication: initial response");
            let response = self.0.initial_response.clone();
            Box::pin(async move { Ok(response) })
        }

        fn handle_challenge<'a>(
            &'a self,
            auth_method: &'a str,
            challenge_data: Option<&'a [u8]>,
        ) -> AuthFuture<'a, AuthResponse> {
            tracing::debug!(
                auth_method,
                ?challenge_data,
                "scripted authentication: challenge"
            );
            let response = self.0.challenge_response.clone();
            Box::pin(async move { Ok(AuthResponse::Continue(response)) })
        }
    }

    fn qos(value: u8) -> QoS {
        match value {
            0 => QoS::AtMostOnce,
            1 => QoS::AtLeastOnce,
            _ => QoS::ExactlyOnce,
        }
    }

    fn client_error(error: &mqtt5::MqttError) -> ClientError {
        ClientError(format!("{error:?}"))
    }

    fn connect_options(spec: &ConnectSpec) -> ConnectOptions {
        let mut options = ConnectOptions::new(spec.client_id.clone())
            .with_keep_alive(spec.keep_alive)
            .with_clean_start(spec.clean_start)
            .with_automatic_reconnect(spec.reconnect_delay.is_some());
        if let Some((initial, max)) = spec.reconnect_delay {
            options = options.with_reconnect_delay(initial, max);
        }
        if let Some(interval) = spec.session_expiry_interval {
            options = options.with_session_expiry_interval(interval);
        }
        if let Some(will) = &spec.will {
            options = options.with_will(
                WillMessage::new(will.topic.clone(), will.payload.clone()).with_qos(qos(will.qos)),
            );
        }
        if let Some((username, password)) = &spec.credentials {
            options = options.with_credentials(username.clone(), password);
        }
        if let Some(authentication) = &spec.authentication {
            options.properties.authentication_method = Some(authentication.method.clone());
        }
        options.properties.request_problem_information = spec.request_problem_information;
        if let Some(receive_maximum) = spec.receive_maximum {
            options = options.with_receive_maximum(receive_maximum);
        }
        options.properties.topic_alias_maximum = spec.topic_alias_maximum;
        options.properties.maximum_packet_size = spec.maximum_packet_size;
        options.with_deferred_ack(spec.deferred_ack)
    }

    struct Mqtt5Client {
        client: MqttClient,
        options: ConnectOptions,
        authentication: Option<AuthSpec>,
        received: Arc<Mutex<Vec<ReceivedMessage>>>,
        deferred: Arc<Mutex<Vec<AckToken>>>,
    }

    fn subscribe_options(value: u8) -> SubscribeOptions {
        SubscribeOptions {
            qos: qos(value),
            ..SubscribeOptions::default()
        }
    }

    impl Drop for Mqtt5Client {
        fn drop(&mut self) {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                let client = self.client.clone();
                runtime.spawn(async move {
                    if let Err(error) = client.disconnect().await {
                        tracing::debug!(%error, "client under test was not connected when dropped");
                    }
                });
            }
        }
    }

    impl ClientUnderTest for Mqtt5Client {
        fn connect(&self, url: &str) -> ClientFuture<Result<ConnectOutcome, ClientError>> {
            let client = self.client.clone();
            let options = self.options.clone();
            let authentication = self.authentication.clone();
            let url = url.to_owned();
            Box::pin(async move {
                if let Some(authentication) = authentication {
                    client
                        .set_auth_handler(ScriptedAuthHandler(authentication))
                        .await;
                }
                Box::pin(client.connect_with_options(&url, options))
                    .await
                    .map(|result| ConnectOutcome {
                        session_present: result.session_present,
                    })
                    .map_err(|error| client_error(&error))
            })
        }

        fn publish(
            &self,
            topic: &str,
            payload: &[u8],
            spec: PublishSpec,
        ) -> ClientFuture<Result<PublishOutcome, ClientError>> {
            let client = self.client.clone();
            let topic = topic.to_owned();
            let payload = payload.to_vec();
            let options = PublishOptions {
                qos: qos(spec.qos),
                retain: spec.retain,
                properties: PublishProperties {
                    topic_alias: spec.topic_alias,
                    response_topic: spec.response_topic,
                    content_type: spec.content_type,
                    subscription_identifiers: spec.subscription_identifiers,
                    ..PublishProperties::default()
                },
                ..PublishOptions::default()
            };
            Box::pin(async move {
                match client.publish_with_options(topic, payload, options).await {
                    Ok(PublishResult::Sent(_)) => Ok(PublishOutcome::Completed),
                    Ok(PublishResult::Queued(_)) => Ok(PublishOutcome::Pending),
                    Err(error) => Err(client_error(&error)),
                }
            })
        }

        fn subscribe(&self, filter: &str, qos: u8) -> ClientFuture<Result<(), ClientError>> {
            let client = self.client.clone();
            let filter = filter.to_owned();
            let received = Arc::clone(&self.received);
            Box::pin(async move {
                client
                    .subscribe_with_options(filter, subscribe_options(qos), move |message| {
                        received
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(ReceivedMessage {
                                topic: message.topic,
                                payload: message.payload,
                            });
                    })
                    .await
                    .map(|_| ())
                    .map_err(|error| client_error(&error))
            })
        }

        fn subscribe_deferred(
            &self,
            filter: &str,
            qos: u8,
        ) -> ClientFuture<Result<(), ClientError>> {
            let client = self.client.clone();
            let filter = filter.to_owned();
            let received = Arc::clone(&self.received);
            let deferred = Arc::clone(&self.deferred);
            Box::pin(async move {
                client
                    .subscribe_with_ack(filter, subscribe_options(qos), move |publish, token| {
                        deferred
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(token);
                        received
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .push(ReceivedMessage {
                                topic: publish.topic_name.clone(),
                                payload: publish.payload.to_vec(),
                            });
                    })
                    .await
                    .map(|_| ())
                    .map_err(|error| client_error(&error))
            })
        }

        fn settle_deferred(&self, settle: Settle) -> usize {
            let tokens: Vec<AckToken> = self
                .deferred
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .drain(..)
                .collect();
            let settled = tokens.len();
            for token in tokens {
                match settle {
                    Settle::Ack => token.ack(),
                    Settle::Reject(reason) => token.reject(
                        ReasonCode::from_u8(reason).unwrap_or(ReasonCode::UnspecifiedError),
                    ),
                }
            }
            settled
        }

        fn unsubscribe(&self, filter: &str) -> ClientFuture<Result<(), ClientError>> {
            let client = self.client.clone();
            let filter = filter.to_owned();
            Box::pin(async move {
                client
                    .unsubscribe(filter)
                    .await
                    .map_err(|error| client_error(&error))
            })
        }

        fn disconnect(&self) -> ClientFuture<Result<(), ClientError>> {
            let client = self.client.clone();
            Box::pin(async move {
                client
                    .disconnect()
                    .await
                    .map_err(|error| client_error(&error))
            })
        }

        fn reauthenticate(&self) -> ClientFuture<Result<(), ClientError>> {
            let client = self.client.clone();
            Box::pin(async move {
                client
                    .reauthenticate()
                    .await
                    .map_err(|error| client_error(&error))
            })
        }

        fn is_connected(&self) -> ClientFuture<bool> {
            let client = self.client.clone();
            Box::pin(async move { client.is_connected().await })
        }

        fn received(&self) -> Vec<ReceivedMessage> {
            self.received
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    struct Mqtt5ClientSut;

    impl ClientSut for Mqtt5ClientSut {
        fn name(&self) -> &'static str {
            "mqtt5"
        }

        fn capabilities(&self) -> ClientCapabilities {
            ClientCapabilities {
                supported: &[
                    ClientRequirement::AutomaticReconnect,
                    ClientRequirement::OfflineQueue,
                    ClientRequirement::EnhancedAuth,
                    ClientRequirement::DeferredAck,
                ],
            }
        }

        fn create(&self, spec: ConnectSpec) -> Arc<dyn ClientUnderTest> {
            let options = connect_options(&spec);
            Arc::new(Mqtt5Client {
                client: MqttClient::with_options(options.clone()),
                options,
                authentication: spec.authentication,
                received: Arc::new(Mutex::new(Vec::new())),
                deferred: Arc::new(Mutex::new(Vec::new())),
            })
        }
    }

    #[must_use]
    pub fn inprocess_client() -> ClientSutHandle {
        Arc::new(Mqtt5ClientSut)
    }
}
