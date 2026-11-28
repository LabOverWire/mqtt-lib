#![cfg(feature = "broker")]

use mqtt5::broker::auth::{
    AuthProvider, AuthRateLimiter, AuthResult, CompositeAuthProvider, EnhancedAuthResult,
    EnhancedAuthStatus, RateLimitedAuthProvider,
};
use mqtt5::error::Result;
use mqtt5::packet::connect::ConnectPacket;
use mqtt5::protocol::v5::reason_codes::ReasonCode;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

struct AlwaysFails;

impl AuthProvider for AlwaysFails {
    fn authenticate<'a>(
        &'a self,
        _: &'a ConnectPacket,
        _: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async { Ok(AuthResult::fail(ReasonCode::NotAuthorized)) })
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
        _: Option<&'a [u8]>,
        _: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        Box::pin(async move {
            Ok(EnhancedAuthResult::fail(
                auth_method,
                ReasonCode::NotAuthorized,
            ))
        })
    }
}

#[tokio::test]
async fn a_composite_forwards_the_client_address_to_a_limited_primary() {
    let limited: Arc<dyn AuthProvider> = Arc::new(RateLimitedAuthProvider::new(
        Arc::new(AlwaysFails),
        Arc::new(AuthRateLimiter::new(2, 60, 60)),
    ));
    let composite = CompositeAuthProvider::new(limited, Arc::new(AlwaysFails));
    let addr: SocketAddr = "192.0.2.1:1000".parse().unwrap();
    for _ in 0..2 {
        let result = composite
            .authenticate_enhanced_from("M", Some(b"x"), "c", addr)
            .await
            .unwrap();
        assert_eq!(result.status, EnhancedAuthStatus::Failed);
    }
    let result = composite
        .authenticate_enhanced_from("M", Some(b"x"), "c", addr)
        .await
        .unwrap();
    assert_eq!(result.reason_code, ReasonCode::ConnectionRateExceeded);
}
