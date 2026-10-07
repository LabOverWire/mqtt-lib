#![cfg(feature = "broker")]

use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use mqtt5::broker::config::{BrokerConfig, StorageBackend, StorageConfig};
use mqtt5::broker::events::{BrokerEventHandler, ClientConnectEvent};
use mqtt5::broker::{BrokerShutdownHandle, MqttBroker};
use mqtt5::client::{AuthHandler, AuthResponse};
use mqtt5::error::Result;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::{ConnectOptions, MqttClient};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

const METHOD: &str = "CHALLENGE-RESPONSE";
const CHALLENGE: &[u8] = b"server-challenge";
const RESPONSE: &[u8] = b"client-response";

struct CleanStartRecorder(Arc<Mutex<Vec<bool>>>);

impl BrokerEventHandler for CleanStartRecorder {
    fn on_client_connect<'a>(
        &'a self,
        event: ClientConnectEvent,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        self.0
            .lock()
            .expect("recorder lock")
            .push(event.clean_start);
        Box::pin(async {})
    }
}

struct ChallengeProvider;

impl AuthProvider for ChallengeProvider {
    fn authenticate<'a>(
        &'a self,
        _: &'a ConnectPacket,
        _: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async { Ok(AuthResult::success()) })
    }

    fn authorize_publish<'a>(
        &'a self,
        _: &str,
        _: Option<&'a str>,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _: &str,
        _: Option<&'a str>,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async { true })
    }

    fn supports_enhanced_auth(&self) -> bool {
        true
    }

    fn authenticate_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        Box::pin(async move {
            match auth_data {
                None => Ok(EnhancedAuthResult::continue_auth(
                    method,
                    Some(CHALLENGE.to_vec()),
                )),
                Some(response) if response == RESPONSE => Ok(EnhancedAuthResult::success(method)),
                Some(_) => Ok(EnhancedAuthResult::fail(method, ReasonCode::NotAuthorized)),
            }
        })
    }

    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
        _: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        self.authenticate_enhanced(auth_method, auth_data, client_id)
    }
}

struct ChallengeAnswer;

impl AuthHandler for ChallengeAnswer {
    fn handle_challenge<'a>(
        &'a self,
        _: &'a str,
        challenge_data: Option<&'a [u8]>,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResponse>> + Send + 'a>> {
        Box::pin(async move {
            if challenge_data == Some(CHALLENGE) {
                Ok(AuthResponse::Continue(RESPONSE.to_vec()))
            } else {
                Ok(AuthResponse::Abort("unexpected challenge".to_string()))
            }
        })
    }
}

struct RecordingBroker {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<bool>>>,
    shutdown: BrokerShutdownHandle,
    run: JoinHandle<Result<()>>,
}

impl RecordingBroker {
    async fn start(enhanced_auth: bool) -> Self {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let config = BrokerConfig::default()
            .with_bind_address("127.0.0.1:0".parse::<SocketAddr>().expect("bind address"))
            .with_storage(StorageConfig::new().with_backend(StorageBackend::Memory))
            .with_event_handler(Arc::new(CleanStartRecorder(Arc::clone(&seen))));
        let broker = MqttBroker::with_config(config).await.expect("start broker");
        let mut broker = if enhanced_auth {
            broker.with_auth_provider(Arc::new(ChallengeProvider))
        } else {
            broker
        };
        let addr = broker.local_addr().expect("broker address");
        let mut ready = broker.ready_receiver();
        let shutdown = broker.shutdown_handle();
        let run = tokio::spawn(async move { broker.run().await });
        ready.wait_for(|&up| up).await.expect("broker ready");
        Self {
            addr,
            seen,
            shutdown,
            run,
        }
    }

    async fn stop(self) -> Vec<bool> {
        self.shutdown.shutdown();
        self.run
            .await
            .expect("broker task joins")
            .expect("broker run returns cleanly");
        let recorded = self.seen.lock().expect("recorder lock").clone();
        recorded
    }
}

async fn recorded_clean_start(sent: &[bool], enhanced_auth: bool) -> Vec<bool> {
    let broker = RecordingBroker::start(enhanced_auth).await;
    let client_id = if enhanced_auth {
        "connect-event-auth"
    } else {
        "connect-event"
    };
    for &clean_start in sent {
        let mut options = ConnectOptions::new(client_id)
            .with_clean_start(clean_start)
            .with_session_expiry_interval(3600)
            .with_resume_existing_session(true);
        if enhanced_auth {
            options = options.with_authentication_method(METHOD);
        }
        let client = MqttClient::with_options(options.clone());
        if enhanced_auth {
            client.set_auth_handler(ChallengeAnswer).await;
        }
        Box::pin(client.connect_with_options(&format!("mqtt://{}", broker.addr), options))
            .await
            .expect("client connects");
        client.disconnect().await.expect("client disconnects");
    }
    broker.stop().await
}

async fn connect_without_expiry(addr: SocketAddr, client_id: &str) -> (MqttClient, bool) {
    let options = ConnectOptions::new(client_id)
        .with_clean_start(false)
        .with_session_expiry_interval(0)
        .with_resume_existing_session(true)
        .with_automatic_reconnect(false);
    let client = MqttClient::with_options(options.clone());
    let connected = Box::pin(client.connect_with_options(&format!("mqtt://{addr}"), options))
        .await
        .expect("client connects");
    (client, connected.session_present)
}

#[tokio::test]
async fn connect_event_reports_the_clean_start_flag_the_client_sent() {
    let sent = [false, false, true];
    assert_eq!(recorded_clean_start(&sent, false).await, sent);
}

#[tokio::test]
async fn connect_event_reports_the_clean_start_flag_after_enhanced_auth() {
    let sent = [false, false, true];
    assert_eq!(recorded_clean_start(&sent, true).await, sent);
}

#[tokio::test]
async fn connect_event_reports_the_clean_start_flag_when_the_stored_session_has_no_expiry() {
    let broker = RecordingBroker::start(false).await;
    let (first, first_resumed) = connect_without_expiry(broker.addr, "connect-event-zero").await;
    let (second, second_resumed) = connect_without_expiry(broker.addr, "connect-event-zero").await;
    assert!(!first_resumed);
    assert!(!second_resumed);
    second.disconnect().await.expect("client disconnects");
    drop(first);
    assert_eq!(broker.stop().await, [false, false]);
}
