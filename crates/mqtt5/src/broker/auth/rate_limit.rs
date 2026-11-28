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

use super::{AuthProvider, AuthResult, EnhancedAuthResult, EnhancedAuthStatus, ReasonCode};

type EnhancedAuthFuture<'a> = Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>>;

#[derive(Debug, Clone)]
struct RateLimitEntry {
    attempts: u32,
    window_start: Instant,
}

pub struct AuthRateLimiter {
    ip_entries: Arc<Mutex<HashMap<IpAddr, RateLimitEntry>>>,
    username_entries: Arc<Mutex<HashMap<String, RateLimitEntry>>>,
    last_prune: Mutex<Instant>,
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
            last_prune: Mutex::new(Instant::now()),
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
        self.prune_if_due();
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
        self.prune_if_due();
        if !success {
            self.record_entry(&self.ip_entries, addr, false);
        }
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
        self.prune(Instant::now());
    }

    fn prune_if_due(&self) {
        let now = Instant::now();
        {
            let mut last_prune = self.last_prune.lock();
            if now.duration_since(*last_prune) < self.window_duration {
                return;
            }
            *last_prune = now;
        }
        self.prune(now);
    }

    fn prune(&self, now: Instant) {
        self.ip_entries
            .lock()
            .retain(|_, entry| self.is_live(entry, now));
        self.username_entries
            .lock()
            .retain(|_, entry| self.is_live(entry, now));
    }

    fn is_live(&self, entry: &RateLimitEntry, now: Instant) -> bool {
        let lifetime = if entry.attempts >= self.max_attempts {
            self.lockout_duration
        } else {
            self.window_duration
        };
        now.duration_since(entry.window_start) < lifetime
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

    fn limit_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        client_addr: SocketAddr,
        exchange: impl FnOnce() -> EnhancedAuthFuture<'a> + Send + 'a,
    ) -> EnhancedAuthFuture<'a> {
        Box::pin(async move {
            let ip = client_addr.ip();
            if !self.rate_limiter.check_rate_limit(ip) {
                return Ok(EnhancedAuthResult::fail_with_reason(
                    auth_method,
                    ReasonCode::ConnectionRateExceeded,
                    "Rate limit exceeded",
                ));
            }
            let result = exchange().await?;
            if result.status == EnhancedAuthStatus::Failed {
                self.rate_limiter.record_attempt(ip, false);
            }
            Ok(result)
        })
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

    fn checks_delivery(&self) -> bool {
        self.inner.checks_delivery()
    }

    fn authorize_delivery<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        self.inner.authorize_delivery(client_id, user_id, topic)
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
        self.inner
            .authenticate_enhanced(auth_method, auth_data, client_id)
    }

    fn authenticate_enhanced_from<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
        client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        self.limit_enhanced(auth_method, client_addr, move || {
            self.inner
                .authenticate_enhanced_from(auth_method, auth_data, client_id, client_addr)
        })
    }

    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
        user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        self.inner
            .reauthenticate(auth_method, auth_data, client_id, user_id)
    }

    fn reauthenticate_from<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
        user_id: Option<&'a str>,
        client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        self.limit_enhanced(auth_method, client_addr, move || {
            self.inner
                .reauthenticate_from(auth_method, auth_data, client_id, user_id, client_addr)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    fn address(index: u16) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, index))
    }

    fn tracked(limiter: &AuthRateLimiter) -> (usize, usize) {
        (
            limiter.ip_entries.lock().len(),
            limiter.username_entries.lock().len(),
        )
    }

    #[test]
    fn entries_no_longer_checked_are_pruned_after_the_window() {
        let limiter = AuthRateLimiter::new(3, 1, 1);
        for index in 0..100 {
            limiter.record_attempt_with_username(
                address(index),
                Some(&format!("user-{index}")),
                false,
            );
        }
        assert_eq!(tracked(&limiter), (100, 100));
        std::thread::sleep(Duration::from_millis(1100));
        assert!(limiter.check_rate_limit_with_username(address(1000), Some("someone-else")));
        assert_eq!(
            tracked(&limiter),
            (0, 0),
            "entries whose window ended must be removed without their key being checked again"
        );
    }

    #[test]
    fn locked_entries_outlive_the_window_until_the_lockout_ends() {
        let limiter = AuthRateLimiter::new(2, 1, 30);
        limiter.record_attempt(address(1), false);
        limiter.record_attempt(address(1), false);
        limiter.record_attempt(address(2), false);
        std::thread::sleep(Duration::from_millis(1100));
        limiter.record_attempt(address(3), false);
        assert_eq!(tracked(&limiter), (2, 0));
        assert!(!limiter.check_rate_limit(address(1)));
        assert!(limiter.check_rate_limit(address(2)));
    }

    #[test]
    fn cleanup_expired_keeps_counting_entries_inside_their_window() {
        let limiter = AuthRateLimiter::new(5, 30, 1);
        limiter.record_attempt(address(1), false);
        std::thread::sleep(Duration::from_millis(1100));
        limiter.cleanup_expired();
        assert_eq!(tracked(&limiter), (1, 0));
    }
}
