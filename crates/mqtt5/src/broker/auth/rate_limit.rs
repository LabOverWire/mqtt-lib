//! Rate limiting for authentication attempts

use crate::error::Result;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::warn;

use crate::packet::connect::ConnectPacket;

use super::{AuthProvider, AuthResult, EnhancedAuthResult, ReasonCode};

#[derive(Debug, Clone)]
struct RateLimitEntry {
    attempts: u32,
    window_start: Instant,
}

pub struct AuthRateLimiter {
    entries: Arc<Mutex<HashMap<IpAddr, RateLimitEntry>>>,
    max_attempts: u32,
    window_duration: Duration,
    lockout_duration: Duration,
}

impl AuthRateLimiter {
    #[must_use]
    pub fn new(max_attempts: u32, window_secs: u64, lockout_secs: u64) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            max_attempts,
            window_duration: Duration::from_secs(window_secs),
            lockout_duration: Duration::from_secs(lockout_secs),
        }
    }

    pub fn check_rate_limit(&self, addr: IpAddr) -> bool {
        let mut entries = self.entries.lock();
        let now = Instant::now();

        if let Some(entry) = entries.get(&addr) {
            if entry.attempts >= self.max_attempts {
                if now.duration_since(entry.window_start) < self.lockout_duration {
                    warn!(ip = %addr, "rate limit exceeded, rejecting authentication");
                    return false;
                }
                entries.remove(&addr);
            } else if now.duration_since(entry.window_start) >= self.window_duration {
                entries.remove(&addr);
            }
        }
        true
    }

    pub fn record_attempt(&self, addr: IpAddr, success: bool) {
        if success {
            self.entries.lock().remove(&addr);
            return;
        }

        let mut entries = self.entries.lock();
        let now = Instant::now();

        let entry = entries.entry(addr).or_insert(RateLimitEntry {
            attempts: 0,
            window_start: now,
        });

        if now.duration_since(entry.window_start) >= self.window_duration {
            entry.attempts = 1;
            entry.window_start = now;
        } else {
            entry.attempts += 1;
        }
    }

    pub fn cleanup_expired(&self) {
        let mut entries = self.entries.lock();
        let now = Instant::now();
        entries.retain(|_, entry| now.duration_since(entry.window_start) < self.lockout_duration);
    }
}

impl Default for AuthRateLimiter {
    fn default() -> Self {
        Self::new(5, 60, 300)
    }
}

pub struct RateLimitedAuthProvider {
    inner: Arc<dyn AuthProvider>,
    rate_limiter: Arc<AuthRateLimiter>,
}

impl RateLimitedAuthProvider {
    pub fn new(inner: Arc<dyn AuthProvider>, rate_limiter: Arc<AuthRateLimiter>) -> Self {
        Self {
            inner,
            rate_limiter,
        }
    }

    pub fn with_default_limits(inner: Arc<dyn AuthProvider>) -> Self {
        Self::new(inner, Arc::new(AuthRateLimiter::default()))
    }
}

impl AuthProvider for RateLimitedAuthProvider {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move {
            let ip = client_addr.ip();

            if !self.rate_limiter.check_rate_limit(ip) {
                return Ok(AuthResult::fail_with_reason(
                    ReasonCode::ConnectionRateExceeded,
                    "Rate limit exceeded".to_string(),
                ));
            }

            let result = self.inner.authenticate(connect, client_addr).await?;
            self.rate_limiter.record_attempt(ip, result.authenticated);
            Ok(result)
        })
    }

    fn authorize_publish<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        let client_id = client_id.to_string();
        Box::pin(async move {
            self.inner
                .authorize_publish(&client_id, user_id, topic)
                .await
        })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        let client_id = client_id.to_string();
        Box::pin(async move {
            self.inner
                .authorize_subscribe(&client_id, user_id, topic_filter)
                .await
        })
    }

    fn supports_enhanced_auth(&self) -> bool {
        self.inner.supports_enhanced_auth()
    }

    fn authenticate_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        Box::pin(async move {
            self.inner
                .authenticate_enhanced(auth_method, auth_data, client_id)
                .await
        })
    }

    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
        user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        Box::pin(async move {
            self.inner
                .reauthenticate(auth_method, auth_data, client_id, user_id)
                .await
        })
    }

    fn cleanup_session<'a>(
        &'a self,
        user_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.inner.cleanup_session(user_id).await;
        })
    }
}
