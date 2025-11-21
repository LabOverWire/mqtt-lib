//! MQTT v5.0 Broker with Enhanced Authentication Example
//!
//! This example demonstrates how to implement enhanced authentication
//! using the AUTH packet for challenge-response flows (like SCRAM-SHA-256).
//!
//! The example shows:
//! - Custom AuthProvider with enhanced auth support
//! - Challenge-response authentication flow
//! - Re-authentication during session
//!
//! ## How Enhanced Auth Works
//!
//! 1. Client sends CONNECT with authentication_method property
//! 2. Broker responds with AUTH (ContinueAuthentication) + challenge
//! 3. Client sends AUTH (ContinueAuthentication) + response
//! 4. Broker sends AUTH (Success) or CONNACK
//!
//! ## Usage
//!
//! ```bash
//! cargo run --example broker_with_enhanced_auth
//! ```
//!
//! Note: Standard MQTT clients (mosquitto_pub/sub) don't support enhanced auth.
//! You would need a client that implements the AUTH packet flow.

use mqtt5::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use mqtt5::broker::{BrokerConfig, MqttBroker};
use mqtt5::error::Result;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use mqtt5::time::Duration;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tracing::{debug, error, info};

pub struct ChallengeResponseAuthProvider {
    challenge: Vec<u8>,
    expected_response: Vec<u8>,
}

impl ChallengeResponseAuthProvider {
    pub fn new(challenge: Vec<u8>, expected_response: Vec<u8>) -> Self {
        Self {
            challenge,
            expected_response,
        }
    }
}

impl AuthProvider for ChallengeResponseAuthProvider {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        let client_id = connect.client_id.clone();
        Box::pin(async move {
            info!("Basic auth for client: {}", client_id);
            Ok(AuthResult::success())
        })
    }

    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move { Ok(true) })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move { Ok(true) })
    }

    fn supports_enhanced_auth(&self) -> bool {
        true
    }

    fn authenticate_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        let challenge = self.challenge.clone();
        let expected = self.expected_response.clone();
        let client = client_id.to_string();

        Box::pin(async move {
            if method != "CHALLENGE-RESPONSE" {
                info!("Client {} used unsupported auth method: {}", client, method);
                return Ok(EnhancedAuthResult::fail(
                    method,
                    ReasonCode::BadAuthenticationMethod,
                ));
            }

            match auth_data {
                None => {
                    info!("Client {} starting auth, sending challenge", client);
                    Ok(EnhancedAuthResult::continue_auth(method, Some(challenge)))
                }
                Some(response) if response == expected => {
                    info!("Client {} authenticated successfully", client);
                    Ok(EnhancedAuthResult::success(method))
                }
                Some(_) => {
                    info!("Client {} failed auth - wrong response", client);
                    Ok(EnhancedAuthResult::fail(method, ReasonCode::NotAuthorized))
                }
            }
        })
    }

    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
        _user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        debug!("Re-authentication requested for client {}", client_id);
        self.authenticate_enhanced(auth_method, auth_data, client_id)
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("mqtt5=info")),
        )
        .init();

    println!("🔐 MQTT v5.0 Enhanced Authentication Example");
    println!("=============================================\n");

    let config = BrokerConfig {
        bind_addresses: vec!["127.0.0.1:1883".parse::<SocketAddr>()?],
        max_clients: 100,
        session_expiry_interval: Duration::from_secs(3600),
        ..Default::default()
    };

    let auth_provider = Arc::new(ChallengeResponseAuthProvider::new(
        b"server-challenge-12345".to_vec(),
        b"correct-response-67890".to_vec(),
    ));

    let mut broker = MqttBroker::with_config(config)
        .await?
        .with_auth_provider(auth_provider);

    info!("✅ Broker with enhanced auth listening on 127.0.0.1:1883");
    info!("🔑 Auth method: CHALLENGE-RESPONSE");
    info!("📋 Challenge: server-challenge-12345");
    info!("📋 Expected response: correct-response-67890");
    info!("🛑 Press Ctrl+C to stop\n");

    let shutdown = tokio::spawn(async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for ctrl-c");
        info!("\n⚠️  Shutdown signal received");
    });

    tokio::select! {
        result = broker.run() => {
            match result {
                Ok(()) => info!("Broker stopped normally"),
                Err(e) => error!("Broker error: {}", e),
            }
        }
        _ = shutdown => {
            info!("Shutting down broker...");
        }
    }

    info!("✅ Broker shutdown complete");
    Ok(())
}
