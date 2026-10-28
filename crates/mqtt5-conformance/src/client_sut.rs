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
    pub fn reconnecting(mut self) -> Self {
        self.reconnect_delay = Some((Duration::from_millis(100), Duration::from_millis(300)));
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
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PublishSpec {
    pub qos: u8,
    pub retain: bool,
    pub topic_alias: Option<u16>,
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
    fn subscribe(&self, filter: &str) -> ClientFuture<Result<(), ClientError>>;
    fn unsubscribe(&self, filter: &str) -> ClientFuture<Result<(), ClientError>>;
    fn disconnect(&self) -> ClientFuture<Result<(), ClientError>>;
    fn reauthenticate(&self) -> ClientFuture<Result<(), ClientError>>;
    fn is_connected(&self) -> ClientFuture<bool>;
    fn received(&self) -> Vec<ReceivedMessage>;
}

#[derive(Debug, Clone, Copy)]
pub struct ClientCapabilities {
    pub automatic_reconnect: bool,
    pub offline_queue: bool,
    pub enhanced_auth: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientRequirement {
    AutomaticReconnect,
    OfflineQueue,
    EnhancedAuth,
}

impl ClientRequirement {
    #[must_use]
    pub fn matches(self, capabilities: &ClientCapabilities) -> bool {
        match self {
            Self::AutomaticReconnect => capabilities.automatic_reconnect,
            Self::OfflineQueue => capabilities.offline_queue,
            Self::EnhancedAuth => capabilities.enhanced_auth,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AutomaticReconnect => "client.automatic_reconnect",
            Self::OfflineQueue => "client.offline_queue",
            Self::EnhancedAuth => "client.enhanced_auth",
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
        AuthSpec, ClientCapabilities, ClientError, ClientFuture, ClientSut, ClientSutHandle,
        ClientUnderTest, ConnectOutcome, ConnectSpec, PublishOutcome, PublishSpec, ReceivedMessage,
    };
    use mqtt5::{
        AuthHandler, AuthResponse, ConnectOptions, MqttClient, PublishOptions, PublishProperties,
        PublishResult, QoS, WillMessage,
    };
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex, PoisonError};

    type AuthFuture<'a, T> = Pin<Box<dyn Future<Output = mqtt5::Result<T>> + Send + 'a>>;

    struct ScriptedAuthHandler(AuthSpec);

    impl AuthHandler for ScriptedAuthHandler {
        fn initial_response<'a>(
            &'a self,
            _auth_method: &'a str,
        ) -> AuthFuture<'a, Option<Vec<u8>>> {
            let response = self.0.initial_response.clone();
            Box::pin(async move { Ok(response) })
        }

        fn handle_challenge<'a>(
            &'a self,
            _auth_method: &'a str,
            _challenge_data: Option<&'a [u8]>,
        ) -> AuthFuture<'a, AuthResponse> {
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
        options
    }

    struct Mqtt5Client {
        client: MqttClient,
        options: ConnectOptions,
        authentication: Option<AuthSpec>,
        received: Arc<Mutex<Vec<ReceivedMessage>>>,
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

        fn subscribe(&self, filter: &str) -> ClientFuture<Result<(), ClientError>> {
            let client = self.client.clone();
            let filter = filter.to_owned();
            let received = Arc::clone(&self.received);
            Box::pin(async move {
                client
                    .subscribe(filter, move |message| {
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
                automatic_reconnect: true,
                offline_queue: true,
                enhanced_auth: true,
            }
        }

        fn create(&self, spec: ConnectSpec) -> Arc<dyn ClientUnderTest> {
            let options = connect_options(&spec);
            Arc::new(Mqtt5Client {
                client: MqttClient::with_options(options.clone()),
                options,
                authentication: spec.authentication,
                received: Arc::new(Mutex::new(Vec::new())),
            })
        }
    }

    #[must_use]
    pub fn inprocess_client() -> ClientSutHandle {
        Arc::new(Mqtt5ClientSut)
    }
}
