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
    ip_entries: Arc<Mutex<HashMap<IpAddr, RateLimitEntry>>>,
    username_entries: Arc<Mutex<HashMap<String, RateLimitEntry>>>,
    max_attempts: u32,
    window_duration: Duration,
    lockout_duration: Duration,
}

impl AuthRateLimiter {
    #[must_use]
    pub fn new(max_attempts: u32, window_secs: u64, lockout_secs: u64) -> Self {
        Self {
            ip_entries: Arc::new(Mutex::new(HashMap::new())),
            username_entries: Arc::new(Mutex::new(HashMap::new())),
            max_attempts,
            window_duration: Duration::from_secs(window_secs),
            lockout_duration: Duration::from_secs(lockout_secs),
        }
    }

    #[must_use]
    pub fn check_rate_limit(&self, addr: IpAddr) -> bool {
        self.check_rate_limit_with_username(addr, None)
    }

    #[must_use]
    pub fn check_rate_limit_with_username(&self, addr: IpAddr, username: Option<&str>) -> bool {
        if !self.check_entry_map(&self.ip_entries, &addr) {
            return false;
        }
        if let Some(name) = username {
            if !self.check_entry_map(&self.username_entries, &name.to_string()) {
                warn!(username = %name, "rate limit exceeded for username, rejecting authentication");
                return false;
            }
        }
        true
    }

    fn check_entry_map<K: std::hash::Hash + Eq + std::fmt::Display>(
        &self,
        map: &Arc<Mutex<HashMap<K, RateLimitEntry>>>,
        key: &K,
    ) -> bool {
        let mut entries = map.lock();
        let now = Instant::now();

        if let Some(entry) = entries.get(key) {
            if entry.attempts >= self.max_attempts {
                if now.duration_since(entry.window_start) < self.lockout_duration {
                    warn!(key = %key, "rate limit exceeded, rejecting authentication");
                    return false;
                }
                entries.remove(key);
            } else if now.duration_since(entry.window_start) >= self.window_duration {
                entries.remove(key);
            }
        }
        true
    }

    pub fn record_attempt(&self, addr: IpAddr, success: bool) {
        self.record_attempt_with_username(addr, None, success);
    }

    pub fn record_attempt_with_username(
        &self,
        addr: IpAddr,
        username: Option<&str>,
        success: bool,
    ) {
        self.record_entry(&self.ip_entries, addr, success);
        if let Some(name) = username {
            self.record_entry(&self.username_entries, name.to_string(), success);
        }
    }

    fn record_entry<K: std::hash::Hash + Eq>(
        &self,
        map: &Arc<Mutex<HashMap<K, RateLimitEntry>>>,
        key: K,
        success: bool,
    ) {
        if success {
            map.lock().remove(&key);
            return;
        }

        let mut entries = map.lock();
        let now = Instant::now();

        let entry = entries.entry(key).or_insert(RateLimitEntry {
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
        let now = Instant::now();
        let lockout = self.lockout_duration;
        self.ip_entries
            .lock()
            .retain(|_, entry| now.duration_since(entry.window_start) < lockout);
        self.username_entries
            .lock()
            .retain(|_, entry| now.duration_since(entry.window_start) < lockout);
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
            let username = connect.username.as_deref();

            if !self
                .rate_limiter
                .check_rate_limit_with_username(ip, username)
            {
                return Ok(AuthResult::fail_with_reason(
                    ReasonCode::ConnectionRateExceeded,
                    "Rate limit exceeded".to_string(),
                ));
            }

            let result = self.inner.authenticate(connect, client_addr).await?;
            self.rate_limiter
                .record_attempt_with_username(ip, username, result.authenticated);
            Ok(result)
        })
    }

    fn authorize_publish<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
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
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
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
