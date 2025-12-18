//! Authentication and authorization for the MQTT broker

use crate::broker::acl::AclManager;
use crate::error::MqttError;
use crate::error::Result;
use crate::packet::connect::ConnectPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, Salt, SaltString};
use argon2::Argon2;
use base64::prelude::*;
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(not(target_arch = "wasm32"))]
use tokio::fs;
use tokio::sync::RwLock;
#[cfg(not(target_arch = "wasm32"))]
use tracing::info;
use tracing::{debug, error, warn};

#[derive(Debug, Clone)]
struct RateLimitEntry {
    attempts: u32,
    window_start: Instant,
}

pub struct AuthRateLimiter {
    entries: Arc<RwLock<HashMap<IpAddr, RateLimitEntry>>>,
    max_attempts: u32,
    window_duration: Duration,
    lockout_duration: Duration,
}

impl AuthRateLimiter {
    pub fn new(max_attempts: u32, window_secs: u64, lockout_secs: u64) -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            max_attempts,
            window_duration: Duration::from_secs(window_secs),
            lockout_duration: Duration::from_secs(lockout_secs),
        }
    }

    pub async fn check_rate_limit(&self, addr: IpAddr) -> bool {
        let mut entries = self.entries.write().await;
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

    pub async fn record_attempt(&self, addr: IpAddr, success: bool) {
        if success {
            self.entries.write().await.remove(&addr);
            return;
        }

        let mut entries = self.entries.write().await;
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

    pub async fn cleanup_expired(&self) {
        let mut entries = self.entries.write().await;
        let now = Instant::now();
        entries.retain(|_, entry| now.duration_since(entry.window_start) < self.lockout_duration);
    }
}

impl Default for AuthRateLimiter {
    fn default() -> Self {
        Self::new(5, 60, 300)
    }
}

/// Authentication result from an auth provider
#[derive(Debug, Clone)]
pub struct AuthResult {
    /// Whether authentication succeeded
    pub authenticated: bool,
    /// Reason code for `ConnAck`
    pub reason_code: ReasonCode,
    /// Optional reason string
    pub reason_string: Option<String>,
    /// User identifier after successful auth
    pub user_id: Option<String>,
}

impl AuthResult {
    /// Creates a successful authentication result
    #[must_use]
    pub fn success() -> Self {
        Self {
            authenticated: true,
            reason_code: ReasonCode::Success,
            reason_string: None,
            user_id: None,
        }
    }

    /// Creates a successful authentication result with user ID
    #[must_use]
    pub fn success_with_user(user_id: String) -> Self {
        Self {
            authenticated: true,
            reason_code: ReasonCode::Success,
            reason_string: None,
            user_id: Some(user_id),
        }
    }

    /// Creates a failed authentication result
    #[must_use]
    pub fn fail(reason_code: ReasonCode) -> Self {
        Self {
            authenticated: false,
            reason_code,
            reason_string: None,
            user_id: None,
        }
    }

    /// Creates a failed authentication result with reason string
    #[must_use]
    pub fn fail_with_reason(reason_code: ReasonCode, reason: String) -> Self {
        Self {
            authenticated: false,
            reason_code,
            reason_string: Some(reason),
            user_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnhancedAuthStatus {
    Success,
    Continue,
    Failed,
}

use super::config::RoleMergeMode;
use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct EnhancedAuthResult {
    pub status: EnhancedAuthStatus,
    pub reason_code: ReasonCode,
    pub auth_method: String,
    pub auth_data: Option<Vec<u8>>,
    pub reason_string: Option<String>,
    pub user_id: Option<String>,
    pub roles: Option<HashSet<String>>,
    pub role_merge_mode: Option<RoleMergeMode>,
}

impl EnhancedAuthResult {
    #[must_use]
    pub fn success(auth_method: String) -> Self {
        Self {
            status: EnhancedAuthStatus::Success,
            reason_code: ReasonCode::Success,
            auth_method,
            auth_data: None,
            reason_string: None,
            user_id: None,
            roles: None,
            role_merge_mode: None,
        }
    }

    #[must_use]
    pub fn success_with_user(auth_method: String, user_id: String) -> Self {
        Self {
            status: EnhancedAuthStatus::Success,
            reason_code: ReasonCode::Success,
            auth_method,
            auth_data: None,
            reason_string: None,
            user_id: Some(user_id),
            roles: None,
            role_merge_mode: None,
        }
    }

    #[must_use]
    pub fn success_with_user_and_roles(
        auth_method: String,
        user_id: String,
        roles: HashSet<String>,
        merge_mode: RoleMergeMode,
    ) -> Self {
        Self {
            status: EnhancedAuthStatus::Success,
            reason_code: ReasonCode::Success,
            auth_method,
            auth_data: None,
            reason_string: None,
            user_id: Some(user_id),
            roles: Some(roles),
            role_merge_mode: Some(merge_mode),
        }
    }

    #[must_use]
    pub fn continue_auth(auth_method: String, auth_data: Option<Vec<u8>>) -> Self {
        Self {
            status: EnhancedAuthStatus::Continue,
            reason_code: ReasonCode::ContinueAuthentication,
            auth_method,
            auth_data,
            reason_string: None,
            user_id: None,
            roles: None,
            role_merge_mode: None,
        }
    }

    #[must_use]
    pub fn fail(auth_method: String, reason_code: ReasonCode) -> Self {
        Self {
            status: EnhancedAuthStatus::Failed,
            reason_code,
            auth_method,
            auth_data: None,
            reason_string: None,
            user_id: None,
            roles: None,
            role_merge_mode: None,
        }
    }

    #[must_use]
    pub fn fail_with_reason(auth_method: String, reason_code: ReasonCode, reason: String) -> Self {
        Self {
            status: EnhancedAuthStatus::Failed,
            reason_code,
            auth_method,
            auth_data: None,
            reason_string: Some(reason),
            user_id: None,
            roles: None,
            role_merge_mode: None,
        }
    }
}

/// Authentication provider trait
pub trait AuthProvider: Send + Sync {
    /// Authenticate a client connection
    ///
    /// # Errors
    ///
    /// Returns an error if authentication check fails (not auth failure)
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>>;

    /// Check if a client is authorized to publish to a topic
    ///
    /// # Errors
    ///
    /// Returns an error if authorization check fails
    fn authorize_publish<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;

    /// Check if a client is authorized to subscribe to a topic filter
    ///
    /// # Errors
    ///
    /// Returns an error if authorization check fails
    fn authorize_subscribe<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>>;

    fn supports_enhanced_auth(&self) -> bool {
        false
    }

    fn authenticate_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        _auth_data: Option<&'a [u8]>,
        _client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        Box::pin(async move {
            Ok(EnhancedAuthResult::fail(
                method,
                ReasonCode::BadAuthenticationMethod,
            ))
        })
    }

    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        _auth_data: Option<&'a [u8]>,
        _client_id: &'a str,
        _user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        let method = auth_method.to_string();
        Box::pin(async move {
            Ok(EnhancedAuthResult::fail(
                method,
                ReasonCode::BadAuthenticationMethod,
            ))
        })
    }

    fn cleanup_session<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async {})
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

            if !self.rate_limiter.check_rate_limit(ip).await {
                return Ok(AuthResult::fail_with_reason(
                    ReasonCode::ConnectionRateExceeded,
                    "Rate limit exceeded".to_string(),
                ));
            }

            let result = self.inner.authenticate(connect, client_addr).await?;
            self.rate_limiter
                .record_attempt(ip, result.authenticated)
                .await;
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

/// Allow all authentication provider (for testing/development)
#[derive(Debug, Clone, Default)]
pub struct AllowAllAuthProvider;

impl AuthProvider for AllowAllAuthProvider {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move { Ok(AuthResult::success()) })
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
}

/// Username/password authentication provider with file loading and Argon2 hashing
#[derive(Debug)]
pub struct PasswordAuthProvider {
    /// Map of username to password hash
    users: Arc<RwLock<HashMap<String, String>>>,
    /// Path to password file (optional)
    password_file: Option<std::path::PathBuf>,
    /// Allow anonymous connections (no username/password)
    allow_anonymous: bool,
}

impl PasswordAuthProvider {
    /// Creates a new password auth provider
    #[must_use]
    pub fn new() -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            password_file: None,
            allow_anonymous: false,
        }
    }

    /// Creates a password auth provider from a file
    ///
    /// File format: `username:password_hash` (one per line)
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let provider = Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            password_file: Some(path.clone()),
            allow_anonymous: false,
        };

        provider.load_password_file().await?;
        Ok(provider)
    }

    /// Sets whether anonymous connections are allowed
    #[must_use]
    pub fn with_anonymous(mut self, allow: bool) -> Self {
        self.allow_anonymous = allow;
        self
    }

    pub fn set_allow_anonymous(&mut self, allow: bool) {
        self.allow_anonymous = allow;
    }

    /// Loads or reloads the password file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn load_password_file(&self) -> Result<()> {
        let Some(ref path) = self.password_file else {
            return Ok(());
        };

        let content = fs::read_to_string(path).await.map_err(|e| {
            MqttError::Configuration(format!(
                "Failed to read password file {}: {}",
                path.display(),
                e
            ))
        })?;

        let mut users = HashMap::new();
        let mut line_num = 0;

        for line in content.lines() {
            line_num += 1;
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse username:password_hash format
            let parts: Vec<&str> = line.splitn(2, ':').collect();
            if parts.len() != 2 {
                warn!(
                    "Invalid format in password file at line {}: {}",
                    line_num, line
                );
                continue;
            }

            let username = parts[0].trim().to_string();
            let password_hash = parts[1].trim().to_string();

            if username.is_empty() {
                warn!("Empty username in password file at line {}", line_num);
                continue;
            }

            users.insert(username, password_hash);
        }

        // Update the users map atomically
        *self.users.write().await = users;

        info!(
            "Loaded {} users from password file: {}",
            self.users.read().await.len(),
            path.display()
        );
        Ok(())
    }

    /// Adds a user with plaintext password (hashes it with Argon2)
    ///
    /// # Errors
    ///
    /// Returns an error if Argon2 hashing fails
    pub async fn add_user(&self, username: String, password: &str) -> Result<()> {
        let password_hash = Self::hash_password(password)?;
        self.users.write().await.insert(username, password_hash);
        Ok(())
    }

    /// Adds a user with pre-hashed password
    pub async fn add_user_with_hash(&self, username: String, password_hash: String) {
        self.users.write().await.insert(username, password_hash);
    }

    /// Removes a user
    pub async fn remove_user(&self, username: &str) -> bool {
        self.users.write().await.remove(username).is_some()
    }

    /// Gets the number of users
    pub async fn user_count(&self) -> usize {
        self.users.read().await.len()
    }

    /// Checks if a user exists
    pub async fn has_user(&self, username: &str) -> bool {
        self.users.read().await.contains_key(username)
    }

    /// Verifies a password for a user
    /// Returns true if the password is valid, false otherwise
    pub async fn verify_user_password(&self, username: &str, password: &str) -> bool {
        let users = self.users.read().await;
        users.get(username).map_or(false, |hash| {
            Self::verify_password(password, hash).unwrap_or(false)
        })
    }

    /// Verifies a password for a user (blocking version)
    /// This is useful for sync contexts
    pub fn verify_user_password_blocking(&self, username: &str, password: &str) -> bool {
        let users = self.users.blocking_read();
        users.get(username).map_or(false, |hash| {
            Self::verify_password(password, hash).unwrap_or(false)
        })
    }
}

impl Default for PasswordAuthProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthProvider for PasswordAuthProvider {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move {
            // Check if username is provided
            let Some(username) = &connect.username else {
                if self.allow_anonymous {
                    debug!("Anonymous connection allowed");
                    return Ok(AuthResult::success());
                }
                return Ok(AuthResult::fail(ReasonCode::BadUsernameOrPassword));
            };

            // Check if password is provided
            let Some(password) = &connect.password else {
                if self.allow_anonymous {
                    debug!("Anonymous connection allowed (username without password)");
                    return Ok(AuthResult::success());
                }
                return Ok(AuthResult::fail(ReasonCode::BadUsernameOrPassword));
            };

            let users = self.users.read().await;
            if let Some(password_hash) = users.get(username) {
                let password_str = String::from_utf8_lossy(password);

                match Self::verify_password(&password_str, password_hash) {
                    Ok(true) => {
                        debug!("Authentication successful for user: {username}");
                        Ok(AuthResult::success_with_user(username.clone()))
                    }
                    Ok(false) => {
                        warn!("Authentication failed for user: {username} (wrong password)");
                        Ok(AuthResult::fail(ReasonCode::BadUsernameOrPassword))
                    }
                    Err(e) => {
                        error!("Argon2 verification error for user {username}: {e}");
                        Ok(AuthResult::fail(ReasonCode::ServerUnavailable))
                    }
                }
            } else {
                warn!("Authentication failed for user: {username} (user not found)");
                Ok(AuthResult::fail(ReasonCode::BadUsernameOrPassword))
            }
        })
    }

    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            // Simple provider allows all authenticated users to publish anywhere
            Ok(true)
        })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            // Simple provider allows all authenticated users to subscribe anywhere
            Ok(true)
        })
    }
}

/// Comprehensive authentication provider with password auth and ACL support
#[derive(Debug)]
pub struct ComprehensiveAuthProvider {
    /// Password authentication
    password_provider: PasswordAuthProvider,
    /// Access control list manager
    acl_manager: AclManager,
}

impl ComprehensiveAuthProvider {
    /// Creates a new comprehensive auth provider
    #[must_use]
    pub fn new() -> Self {
        Self {
            password_provider: PasswordAuthProvider::new(),
            acl_manager: AclManager::new(),
        }
    }

    /// Creates a comprehensive auth provider with existing providers
    #[must_use]
    pub fn with_providers(
        password_provider: PasswordAuthProvider,
        acl_manager: AclManager,
    ) -> Self {
        Self {
            password_provider,
            acl_manager,
        }
    }

    /// Creates a comprehensive auth provider with password file and ACL file
    ///
    /// # Errors
    ///
    /// Returns an error if files cannot be loaded
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn from_files(
        password_file: impl AsRef<Path>,
        acl_file: impl AsRef<Path>,
    ) -> Result<Self> {
        let password_provider = PasswordAuthProvider::from_file(password_file).await?;
        let acl_manager = AclManager::from_file(acl_file).await?;

        Ok(Self {
            password_provider,
            acl_manager,
        })
    }

    /// Creates a comprehensive auth provider with allow-all ACL
    ///
    /// # Errors
    ///
    /// Returns an error if the password file cannot be loaded
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn with_password_file_and_allow_all_acl(
        password_file: impl AsRef<Path>,
    ) -> Result<Self> {
        let password_provider = PasswordAuthProvider::from_file(password_file).await?;
        let acl_manager = AclManager::allow_all();

        Ok(Self {
            password_provider,
            acl_manager,
        })
    }

    /// Gets access to the password provider
    #[must_use]
    pub fn password_provider(&self) -> &PasswordAuthProvider {
        &self.password_provider
    }

    /// Gets access to the ACL manager
    #[must_use]
    pub fn acl_manager(&self) -> &AclManager {
        &self.acl_manager
    }

    /// Reloads password and ACL files
    ///
    /// # Errors
    ///
    /// Returns an error if files cannot be reloaded
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn reload(&self) -> Result<()> {
        self.password_provider.load_password_file().await?;
        self.acl_manager.load_acl_file().await?;
        Ok(())
    }
}

impl Default for ComprehensiveAuthProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthProvider for ComprehensiveAuthProvider {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move {
            // Delegate to password provider for authentication
            self.password_provider
                .authenticate(connect, client_addr)
                .await
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
            debug!(
                "Checking publish authorization for client={}, user={:?}, topic={}",
                client_id, user_id, topic
            );
            let allowed = self.acl_manager.check_publish(user_id, topic).await;
            debug!("Authorization result: {}", allowed);
            Ok(allowed)
        })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        user_id: Option<&'a str>,
        topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            // Use ACL manager for authorization
            Ok(self
                .acl_manager
                .check_subscribe(user_id, topic_filter)
                .await)
        })
    }
}

/// Certificate-based authentication provider using X.509 client certificates
#[derive(Debug)]
pub struct CertificateAuthProvider {
    /// Map of certificate fingerprints to user identifiers
    allowed_certs: Arc<RwLock<HashMap<String, String>>>,
    /// Path to certificate file (optional)
    cert_file: Option<std::path::PathBuf>,
}

impl CertificateAuthProvider {
    /// Creates a new certificate auth provider
    #[must_use]
    pub fn new() -> Self {
        Self {
            allowed_certs: Arc::new(RwLock::new(HashMap::new())),
            cert_file: None,
        }
    }

    /// Creates a certificate auth provider from a file
    ///
    /// File format: `fingerprint:username` (one per line)
    /// Fingerprints are SHA-256 hex strings of the certificate DER bytes
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let provider = Self {
            allowed_certs: Arc::new(RwLock::new(HashMap::new())),
            cert_file: Some(path.clone()),
        };

        provider.load_cert_file().await?;
        Ok(provider)
    }

    /// Loads or reloads the certificate file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed
    #[cfg(not(target_arch = "wasm32"))]
    pub async fn load_cert_file(&self) -> Result<()> {
        let Some(ref path) = self.cert_file else {
            return Ok(());
        };

        let content = fs::read_to_string(path).await.map_err(|e| {
            MqttError::Configuration(format!(
                "Failed to read certificate file {}: {}",
                path.display(),
                e
            ))
        })?;

        let mut certs = HashMap::new();
        let mut line_num = 0;

        for line in content.lines() {
            line_num += 1;
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            // Parse fingerprint:username format
            let parts: Vec<&str> = line.splitn(2, ':').collect();
            if parts.len() != 2 {
                warn!(
                    "Invalid format in certificate file at line {}: {}",
                    line_num, line
                );
                continue;
            }

            let fingerprint = parts[0].trim().to_lowercase();
            let username = parts[1].trim().to_string();

            // Validate fingerprint format (SHA-256 hex)
            if fingerprint.len() != 64 || !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
                warn!(
                    "Invalid fingerprint format at line {}: {}",
                    line_num, fingerprint
                );
                continue;
            }

            if username.is_empty() {
                warn!("Empty username in certificate file at line {}", line_num);
                continue;
            }

            certs.insert(fingerprint, username);
        }

        // Update the certificates map atomically
        *self.allowed_certs.write().await = certs;

        info!(
            "Loaded {} certificate mappings from file: {}",
            self.allowed_certs.read().await.len(),
            path.display()
        );
        Ok(())
    }

    /// Adds an allowed certificate by fingerprint
    pub async fn add_certificate(&self, fingerprint: String, username: String) {
        let normalized_fingerprint = fingerprint.to_lowercase();
        self.allowed_certs
            .write()
            .await
            .insert(normalized_fingerprint, username);
    }

    /// Removes an allowed certificate
    pub async fn remove_certificate(&self, fingerprint: &str) -> bool {
        let normalized_fingerprint = fingerprint.to_lowercase();
        self.allowed_certs
            .write()
            .await
            .remove(&normalized_fingerprint)
            .is_some()
    }

    /// Gets the number of allowed certificates
    pub async fn cert_count(&self) -> usize {
        self.allowed_certs.read().await.len()
    }

    /// Checks if a certificate is allowed
    pub async fn has_certificate(&self, fingerprint: &str) -> bool {
        let normalized_fingerprint = fingerprint.to_lowercase();
        self.allowed_certs
            .read()
            .await
            .contains_key(&normalized_fingerprint)
    }

    /// Calculates SHA-256 fingerprint of a certificate
    #[must_use]
    pub fn calculate_fingerprint(cert_der: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(cert_der);
        hex::encode(hasher.finalize())
    }

    /// Extracts the Common Name (CN) from a certificate subject
    ///
    /// # Errors
    ///
    /// Returns an error if the certificate cannot be parsed
    pub fn extract_common_name(cert_der: &[u8]) -> Result<Option<String>> {
        // Parse the certificate to extract the common name
        // This is a simplified implementation - in production you might want to use
        // a full X.509 parser like the `x509-parser` crate

        // For now, we'll use a basic approach that works with most certificates
        // In a full implementation, you'd want proper ASN.1/DER parsing
        let _cert_pem = format!(
            "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----",
            BASE64_STANDARD.encode(cert_der)
        );

        // Try to extract CN using simple string matching
        // This is a fallback - proper X.509 parsing would be better
        if let Ok(decoded) = std::str::from_utf8(cert_der) {
            // Look for CN= pattern in the certificate data
            if let Some(cn_start) = decoded.find("CN=") {
                let cn_part = &decoded[cn_start + 3..];
                if let Some(cn_end) = cn_part.find(',').or_else(|| cn_part.find('\0')) {
                    return Ok(Some(cn_part[..cn_end].trim().to_string()));
                }
            }
        }

        // If we can't extract CN, return None (still valid for fingerprint-based auth)
        Ok(None)
    }
}

impl Default for CertificateAuthProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthProvider for CertificateAuthProvider {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move {
            // Certificate authentication requires the client certificate to be extracted
            // from the TLS connection context. Since we don't have direct access to that here,
            // we'll need to pass it through a different mechanism.
            //
            // For now, we'll check if a client ID contains certificate information
            // In a full implementation, you'd extract this from the TLS connection

            // This is a placeholder implementation - in production, the certificate
            // would be extracted from the TLS handshake and passed to this method
            debug!(
                "Certificate authentication attempted for client: {} from {}",
                connect.client_id, client_addr
            );

            // For demonstration, we'll look for a special client ID pattern
            // In practice, the certificate would be passed via TLS connection metadata
            if connect.client_id.starts_with("cert:") {
                let fingerprint = &connect.client_id[5..]; // Remove "cert:" prefix

                let certs = self.allowed_certs.read().await;
                if let Some(username) = certs.get(&fingerprint.to_lowercase()) {
                    debug!(
                        "Certificate authentication successful for fingerprint: {}",
                        fingerprint
                    );
                    return Ok(AuthResult::success_with_user(username.clone()));
                }
                warn!(
                    "Certificate authentication failed: unknown fingerprint {}",
                    fingerprint
                );
                return Ok(AuthResult::fail(ReasonCode::NotAuthorized));
            }

            // No certificate provided or invalid format
            warn!(
                "Certificate authentication failed: no valid certificate for client {}",
                connect.client_id
            );
            Ok(AuthResult::fail(ReasonCode::BadUsernameOrPassword))
        })
    }

    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            // Certificate provider allows all authenticated users to publish anywhere
            // In practice, you might want to integrate with ACLs
            Ok(true)
        })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            // Certificate provider allows all authenticated users to subscribe anywhere
            // In practice, you might want to integrate with ACLs
            Ok(true)
        })
    }
}

/// Utility functions for password management
impl PasswordAuthProvider {
    /// Generates an Argon2 hash for a password
    ///
    /// # Errors
    ///
    /// Returns an error if Argon2 hashing fails
    pub fn hash_password(password: &str) -> Result<String> {
        let mut bytes = [0u8; Salt::RECOMMENDED_LENGTH];
        getrandom::fill(&mut bytes).map_err(|e| {
            error!("Failed to generate random salt: {}", e);
            MqttError::AuthenticationFailed
        })?;
        let salt = SaltString::encode_b64(&bytes).map_err(|e| {
            error!("Failed to encode salt: {}", e);
            MqttError::AuthenticationFailed
        })?;
        let argon2 = Argon2::default();
        argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|e| {
                error!("Failed to hash password: {}", e);
                MqttError::AuthenticationFailed
            })
    }

    /// Verifies a password against an Argon2 hash
    ///
    /// # Errors
    ///
    /// Returns an error if Argon2 verification fails
    pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
        let parsed_hash = PasswordHash::new(hash).map_err(|e| {
            error!("Failed to parse password hash: {}", e);
            MqttError::AuthenticationFailed
        })?;
        Ok(Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::connect::ConnectPacket;

    #[tokio::test]
    async fn test_allow_all_provider() {
        let provider = AllowAllAuthProvider;
        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("test-client"));
        let addr = "127.0.0.1:12345".parse().unwrap();

        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::Success);

        let can_publish = provider
            .authorize_publish("test", None, "test/topic")
            .await
            .unwrap();
        assert!(can_publish);

        let can_subscribe = provider
            .authorize_subscribe("test", None, "test/+")
            .await
            .unwrap();
        assert!(can_subscribe);
    }

    #[tokio::test]
    async fn test_password_provider() {
        let provider = PasswordAuthProvider::new();
        provider
            .add_user("alice".to_string(), "secret123")
            .await
            .unwrap();

        let addr = "127.0.0.1:12345".parse().unwrap();

        // Test successful auth
        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("test-client"));
        connect.username = Some("alice".to_string());
        connect.password = Some("secret123".as_bytes().to_vec());

        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        // Test wrong password
        connect.password = Some("wrong".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::BadUsernameOrPassword);

        // Test missing password
        connect.password = None;
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);

        // Test unknown user
        connect.username = Some("bob".to_string());
        connect.password = Some("password".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
    }

    #[test]
    fn test_auth_result_builders() {
        let result = AuthResult::success();
        assert!(result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::Success);
        assert!(result.user_id.is_none());

        let result = AuthResult::success_with_user("alice".to_string());
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        let result = AuthResult::fail(ReasonCode::NotAuthorized);
        assert!(!result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::NotAuthorized);

        let result = AuthResult::fail_with_reason(
            ReasonCode::Banned,
            "Too many failed attempts".to_string(),
        );
        assert!(!result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::Banned);
        assert_eq!(
            result.reason_string,
            Some("Too many failed attempts".to_string())
        );
    }

    #[tokio::test]
    async fn test_password_hashing() {
        let password = "test123";
        let hash = PasswordAuthProvider::hash_password(password).unwrap();

        // Hash should be different from password
        assert_ne!(hash, password);

        // Should be able to verify correct password
        assert!(PasswordAuthProvider::verify_password(password, &hash).unwrap());

        // Should reject wrong password
        assert!(!PasswordAuthProvider::verify_password("wrong", &hash).unwrap());
    }

    #[tokio::test]
    async fn test_async_user_management() {
        let provider = PasswordAuthProvider::new();

        // Initially empty
        assert_eq!(provider.user_count().await, 0);
        assert!(!provider.has_user("alice").await);

        // Add user with plaintext password (gets hashed)
        provider
            .add_user("alice".to_string(), "secret123")
            .await
            .unwrap();
        assert_eq!(provider.user_count().await, 1);
        assert!(provider.has_user("alice").await);

        // Add user with pre-hashed password
        let hash = PasswordAuthProvider::hash_password("password456").unwrap();
        provider.add_user_with_hash("bob".to_string(), hash).await;
        assert_eq!(provider.user_count().await, 2);
        assert!(provider.has_user("bob").await);

        // Remove user
        assert!(provider.remove_user("alice").await);
        assert_eq!(provider.user_count().await, 1);
        assert!(!provider.has_user("alice").await);

        // Remove non-existent user
        assert!(!provider.remove_user("charlie").await);
        assert_eq!(provider.user_count().await, 1);
    }

    #[tokio::test]
    async fn test_file_based_authentication() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create temporary password file
        let mut temp_file = NamedTempFile::new().unwrap();
        let alice_hash = PasswordAuthProvider::hash_password("secret123").unwrap();
        let bob_hash = PasswordAuthProvider::hash_password("password456").unwrap();

        writeln!(temp_file, "# Password file").unwrap();
        writeln!(temp_file, "alice:{}", alice_hash).unwrap();
        writeln!(temp_file, "bob:{}", bob_hash).unwrap();
        writeln!(temp_file, "# Comment line").unwrap();
        writeln!(temp_file, "").unwrap(); // Empty line
        writeln!(temp_file, "invalid_line_without_colon").unwrap();
        temp_file.flush().unwrap();

        // Load from file
        let provider = PasswordAuthProvider::from_file(temp_file.path())
            .await
            .unwrap();
        assert_eq!(provider.user_count().await, 2);
        assert!(provider.has_user("alice").await);
        assert!(provider.has_user("bob").await);

        // Test authentication
        let addr = "127.0.0.1:12345".parse().unwrap();

        // Test Alice
        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("test-client"));
        connect.username = Some("alice".to_string());
        connect.password = Some("secret123".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        // Test Bob
        connect.username = Some("bob".to_string());
        connect.password = Some("password456".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("bob".to_string()));

        // Test wrong password
        connect.password = Some("wrong".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);

        // Test unknown user
        connect.username = Some("charlie".to_string());
        connect.password = Some("password".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
    }

    #[tokio::test]
    async fn test_password_file_reload() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create temporary password file with one user
        let mut temp_file = NamedTempFile::new().unwrap();
        let alice_hash = PasswordAuthProvider::hash_password("secret123").unwrap();
        writeln!(temp_file, "alice:{}", alice_hash).unwrap();
        temp_file.flush().unwrap();

        let provider = PasswordAuthProvider::from_file(temp_file.path())
            .await
            .unwrap();
        assert_eq!(provider.user_count().await, 1);

        // Update file with additional user
        let bob_hash = PasswordAuthProvider::hash_password("password456").unwrap();
        writeln!(temp_file, "bob:{}", bob_hash).unwrap();
        temp_file.flush().unwrap();

        // Reload should pick up the new user
        provider.load_password_file().await.unwrap();
        assert_eq!(provider.user_count().await, 2);
        assert!(provider.has_user("alice").await);
        assert!(provider.has_user("bob").await);
    }

    #[tokio::test]
    async fn test_comprehensive_auth_provider() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create temporary password file
        let mut password_file = NamedTempFile::new().unwrap();
        let alice_hash = PasswordAuthProvider::hash_password("secret123").unwrap();
        let bob_hash = PasswordAuthProvider::hash_password("password456").unwrap();
        writeln!(password_file, "alice:{}", alice_hash).unwrap();
        writeln!(password_file, "bob:{}", bob_hash).unwrap();
        password_file.flush().unwrap();

        // Create temporary ACL file
        let mut acl_file = NamedTempFile::new().unwrap();
        writeln!(acl_file, "user alice topic sensors/+ permission read").unwrap();
        writeln!(acl_file, "user bob topic actuators/# permission write").unwrap();
        writeln!(acl_file, "user * topic public/# permission readwrite").unwrap();
        acl_file.flush().unwrap();

        // Create comprehensive auth provider
        let auth = ComprehensiveAuthProvider::from_files(password_file.path(), acl_file.path())
            .await
            .unwrap();

        let addr = "127.0.0.1:12345".parse().unwrap();

        // Test Alice authentication and authorization
        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("alice-client"));
        connect.username = Some("alice".to_string());
        connect.password = Some("secret123".as_bytes().to_vec());

        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        // Test Alice's permissions
        assert!(!auth
            .authorize_publish("alice-client", Some("alice"), "sensors/temp")
            .await
            .unwrap());
        assert!(auth
            .authorize_subscribe("alice-client", Some("alice"), "sensors/temp")
            .await
            .unwrap());
        assert!(auth
            .authorize_publish("alice-client", Some("alice"), "public/announcements")
            .await
            .unwrap());

        // Test Bob authentication and authorization
        connect.username = Some("bob".to_string());
        connect.password = Some("password456".as_bytes().to_vec());

        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("bob".to_string()));

        // Test Bob's permissions
        assert!(auth
            .authorize_publish("bob-client", Some("bob"), "actuators/fan")
            .await
            .unwrap());
        assert!(!auth
            .authorize_subscribe("bob-client", Some("bob"), "actuators/fan")
            .await
            .unwrap());
        assert!(auth
            .authorize_publish("bob-client", Some("bob"), "public/messages")
            .await
            .unwrap());

        // Test unknown user authentication failure
        connect.username = Some("charlie".to_string());
        connect.password = Some("wrongpass".as_bytes().to_vec());

        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);

        // Test reload functionality
        auth.reload().await.unwrap();
    }

    #[tokio::test]
    async fn test_comprehensive_auth_with_allow_all_acl() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create temporary password file
        let mut password_file = NamedTempFile::new().unwrap();
        let alice_hash = PasswordAuthProvider::hash_password("secret123").unwrap();
        writeln!(password_file, "alice:{}", alice_hash).unwrap();
        password_file.flush().unwrap();

        // Create auth provider with allow-all ACL
        let auth =
            ComprehensiveAuthProvider::with_password_file_and_allow_all_acl(password_file.path())
                .await
                .unwrap();

        let addr = "127.0.0.1:12345".parse().unwrap();

        // Test Alice authentication
        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("alice-client"));
        connect.username = Some("alice".to_string());
        connect.password = Some("secret123".as_bytes().to_vec());

        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);

        // Alice should have full access with allow-all ACL
        assert!(auth
            .authorize_publish("alice-client", Some("alice"), "any/topic")
            .await
            .unwrap());
        assert!(auth
            .authorize_subscribe("alice-client", Some("alice"), "any/topic")
            .await
            .unwrap());

        // Test wrong password
        connect.password = Some("wrong".as_bytes().to_vec());
        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
    }

    #[tokio::test]
    async fn test_certificate_provider() {
        let provider = CertificateAuthProvider::new();
        let fingerprint = "1234567890123456789012345678901234567890123456789012345678901234";
        provider
            .add_certificate(fingerprint.to_string(), "alice".to_string())
            .await;

        let addr = "127.0.0.1:12345".parse().unwrap();

        // Test successful cert auth
        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new(&format!(
            "cert:{}",
            fingerprint
        )));
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        // Test unknown fingerprint
        let unknown_fingerprint =
            "c3d4e5f6789012345678901234567890abcdef123456789012345678901234567b";
        connect.client_id = format!("cert:{}", unknown_fingerprint);
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::NotAuthorized);

        // Test invalid client ID format
        connect.client_id = "not-cert-format".to_string();
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::BadUsernameOrPassword);
    }

    #[test]
    fn test_certificate_fingerprint_calculation() {
        let test_data = b"test certificate data";
        let fingerprint = CertificateAuthProvider::calculate_fingerprint(test_data);

        // Should be a 64-character hex string (SHA-256)
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));

        // Same data should produce same fingerprint
        let fingerprint2 = CertificateAuthProvider::calculate_fingerprint(test_data);
        assert_eq!(fingerprint, fingerprint2);

        // Different data should produce different fingerprint
        let different_data = b"different certificate data";
        let different_fingerprint = CertificateAuthProvider::calculate_fingerprint(different_data);
        assert_ne!(fingerprint, different_fingerprint);
    }

    #[tokio::test]
    async fn test_certificate_management() {
        let provider = CertificateAuthProvider::new();

        // Initially empty
        assert_eq!(provider.cert_count().await, 0);
        assert!(!provider.has_certificate("abc123").await);

        // Add certificate
        let fingerprint = "1234567890123456789012345678901234567890123456789012345678901234";
        provider
            .add_certificate(fingerprint.to_string(), "alice".to_string())
            .await;
        assert_eq!(provider.cert_count().await, 1);
        assert!(provider.has_certificate(fingerprint).await);

        // Case insensitive
        assert!(provider.has_certificate(&fingerprint.to_uppercase()).await);

        // Remove certificate
        assert!(provider.remove_certificate(fingerprint).await);
        assert_eq!(provider.cert_count().await, 0);
        assert!(!provider.has_certificate(fingerprint).await);

        // Remove non-existent certificate
        assert!(!provider.remove_certificate("nonexistent").await);
    }

    #[tokio::test]
    async fn test_certificate_file_loading() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create temporary certificate file
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "# Certificate fingerprint mappings").unwrap();
        // 64-character SHA-256 hex strings
        writeln!(
            temp_file,
            "1234567890123456789012345678901234567890123456789012345678901234:alice"
        )
        .unwrap();
        writeln!(
            temp_file,
            "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd:bob"
        )
        .unwrap();
        writeln!(temp_file, "# Another comment").unwrap();
        writeln!(temp_file, "").unwrap(); // Empty line
        writeln!(temp_file, "invalid:line:too:many:colons").unwrap();
        writeln!(temp_file, "short:charlie").unwrap(); // Invalid fingerprint (too short)
        writeln!(
            temp_file,
            "not_hex_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz:dave"
        )
        .unwrap(); // Invalid hex
        writeln!(
            temp_file,
            "c3d4e5f6789012345678901234567890123456789012345678901234567890abcd:"
        )
        .unwrap(); // Empty username
        temp_file.flush().unwrap();

        // Load from file
        let provider = CertificateAuthProvider::from_file(temp_file.path())
            .await
            .unwrap();
        assert_eq!(provider.cert_count().await, 2); // Only valid entries
        assert!(
            provider
                .has_certificate("1234567890123456789012345678901234567890123456789012345678901234")
                .await
        );
        assert!(
            provider
                .has_certificate("abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd")
                .await
        );

        // Test authentication with loaded certificates
        let addr = "127.0.0.1:12345".parse().unwrap();

        // Test Alice
        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new(
            "cert:1234567890123456789012345678901234567890123456789012345678901234",
        ));
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        // Test Bob
        connect.client_id =
            "cert:abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd".to_string();
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("bob".to_string()));

        // Test unknown certificate
        connect.client_id =
            "cert:unknown12345678901234567890123456789012345678901234567890123456".to_string();
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
    }

    #[tokio::test]
    async fn test_certificate_file_reload() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        // Create temporary certificate file with one certificate
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(
            temp_file,
            "1234567890123456789012345678901234567890123456789012345678901234:alice"
        )
        .unwrap();
        temp_file.flush().unwrap();

        let provider = CertificateAuthProvider::from_file(temp_file.path())
            .await
            .unwrap();
        assert_eq!(provider.cert_count().await, 1);

        // Update file with additional certificate
        writeln!(
            temp_file,
            "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd:bob"
        )
        .unwrap();
        temp_file.flush().unwrap();

        // Reload should pick up the new certificate
        provider.load_cert_file().await.unwrap();
        assert_eq!(provider.cert_count().await, 2);
        assert!(
            provider
                .has_certificate("1234567890123456789012345678901234567890123456789012345678901234")
                .await
        );
        assert!(
            provider
                .has_certificate("abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd")
                .await
        );
    }

    #[test]
    fn test_certificate_common_name_extraction() {
        // This is a basic test - in practice, you'd need real DER-encoded certificates
        // For now, we test the function exists and handles edge cases
        let empty_cert = b"";
        let result = CertificateAuthProvider::extract_common_name(empty_cert);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        // Test with data that contains CN pattern
        let fake_cert_with_cn = b"Some certificate data CN=test.example.com,O=Test Org more data";
        let result = CertificateAuthProvider::extract_common_name(fake_cert_with_cn);
        assert!(result.is_ok());
        // Note: This test might not work perfectly due to the simplified implementation
        // In production, you'd use proper X.509 parsing
    }

    #[tokio::test]
    async fn test_enhanced_auth_result_builders() {
        let result = EnhancedAuthResult::success("SCRAM-SHA-256".to_string());
        assert_eq!(result.status, EnhancedAuthStatus::Success);
        assert_eq!(result.reason_code, ReasonCode::Success);
        assert_eq!(result.auth_method, "SCRAM-SHA-256");
        assert!(result.auth_data.is_none());

        let result = EnhancedAuthResult::continue_auth(
            "SCRAM-SHA-256".to_string(),
            Some(b"challenge".to_vec()),
        );
        assert_eq!(result.status, EnhancedAuthStatus::Continue);
        assert_eq!(result.reason_code, ReasonCode::ContinueAuthentication);
        assert_eq!(result.auth_data, Some(b"challenge".to_vec()));

        let result =
            EnhancedAuthResult::fail("SCRAM-SHA-256".to_string(), ReasonCode::NotAuthorized);
        assert_eq!(result.status, EnhancedAuthStatus::Failed);
        assert_eq!(result.reason_code, ReasonCode::NotAuthorized);

        let result = EnhancedAuthResult::fail_with_reason(
            "SCRAM-SHA-256".to_string(),
            ReasonCode::BadAuthenticationMethod,
            "Unsupported method".to_string(),
        );
        assert_eq!(result.status, EnhancedAuthStatus::Failed);
        assert_eq!(result.reason_string, Some("Unsupported method".to_string()));
    }

    #[tokio::test]
    async fn test_default_enhanced_auth_not_supported() {
        let provider = AllowAllAuthProvider;
        assert!(!provider.supports_enhanced_auth());

        let result = provider
            .authenticate_enhanced("SCRAM-SHA-256", Some(b"data"), "client-1")
            .await
            .unwrap();
        assert_eq!(result.status, EnhancedAuthStatus::Failed);
        assert_eq!(result.reason_code, ReasonCode::BadAuthenticationMethod);

        let result = provider
            .reauthenticate("SCRAM-SHA-256", Some(b"data"), "client-1", Some("user"))
            .await
            .unwrap();
        assert_eq!(result.status, EnhancedAuthStatus::Failed);
        assert_eq!(result.reason_code, ReasonCode::BadAuthenticationMethod);
    }

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
            _connect: &'a ConnectPacket,
            _client_addr: SocketAddr,
        ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
            Box::pin(async move { Ok(AuthResult::success()) })
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
            _client_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
            let method = auth_method.to_string();
            let challenge = self.challenge.clone();
            let expected = self.expected_response.clone();

            Box::pin(async move {
                if method != "CHALLENGE-RESPONSE" {
                    return Ok(EnhancedAuthResult::fail(
                        method,
                        ReasonCode::BadAuthenticationMethod,
                    ));
                }

                match auth_data {
                    None => Ok(EnhancedAuthResult::continue_auth(method, Some(challenge))),
                    Some(response) if response == expected => {
                        Ok(EnhancedAuthResult::success(method))
                    }
                    Some(_) => Ok(EnhancedAuthResult::fail(method, ReasonCode::NotAuthorized)),
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
            self.authenticate_enhanced(auth_method, auth_data, client_id)
        }
    }

    #[tokio::test]
    async fn test_challenge_response_provider() {
        let provider = ChallengeResponseAuthProvider::new(
            b"server-challenge".to_vec(),
            b"correct-response".to_vec(),
        );

        assert!(provider.supports_enhanced_auth());

        let result = provider
            .authenticate_enhanced("CHALLENGE-RESPONSE", None, "client-1")
            .await
            .unwrap();
        assert_eq!(result.status, EnhancedAuthStatus::Continue);
        assert_eq!(result.auth_data, Some(b"server-challenge".to_vec()));

        let result = provider
            .authenticate_enhanced("CHALLENGE-RESPONSE", Some(b"correct-response"), "client-1")
            .await
            .unwrap();
        assert_eq!(result.status, EnhancedAuthStatus::Success);

        let result = provider
            .authenticate_enhanced("CHALLENGE-RESPONSE", Some(b"wrong-response"), "client-1")
            .await
            .unwrap();
        assert_eq!(result.status, EnhancedAuthStatus::Failed);
        assert_eq!(result.reason_code, ReasonCode::NotAuthorized);

        let result = provider
            .authenticate_enhanced("WRONG-METHOD", None, "client-1")
            .await
            .unwrap();
        assert_eq!(result.status, EnhancedAuthStatus::Failed);
        assert_eq!(result.reason_code, ReasonCode::BadAuthenticationMethod);
    }
}
