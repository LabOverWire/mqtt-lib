//! Built-in authentication provider implementations

use crate::broker::acl::AclManager;
use crate::error::{MqttError, Result};
use crate::packet::connect::ConnectPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, Salt, SaltString};
use argon2::Argon2;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use tokio::fs;
#[cfg(not(target_arch = "wasm32"))]
use tracing::info;
use tracing::{debug, error, warn};

use super::{AuthProvider, AuthResult};

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
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }
}

/// Username/password authentication provider with file loading and Argon2 hashing
#[derive(Debug)]
pub struct PasswordAuthProvider {
    users: Arc<RwLock<HashMap<String, String>>>,
    #[cfg(not(target_arch = "wasm32"))]
    password_file: Option<std::path::PathBuf>,
    allow_anonymous: bool,
}

impl PasswordAuthProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(target_arch = "wasm32"))]
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

            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let parts: Vec<&str> = line.splitn(2, ':').collect();
            if parts.len() != 2 {
                warn!("Invalid format in password file at line {line_num}");
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

        *self.users.write() = users;

        info!(
            "Loaded {} users from password file: {}",
            self.users.read().len(),
            path.display()
        );
        Ok(())
    }

    /// Adds a user with plaintext password (hashes it with Argon2)
    ///
    /// # Errors
    ///
    /// Returns an error if Argon2 hashing fails
    pub fn add_user(&self, username: impl Into<String>, password: &str) -> Result<()> {
        let password_hash = Self::hash_password(password)?;
        self.users.write().insert(username.into(), password_hash);
        Ok(())
    }

    pub fn add_user_with_hash(
        &self,
        username: impl Into<String>,
        password_hash: impl Into<String>,
    ) {
        self.users
            .write()
            .insert(username.into(), password_hash.into());
    }

    #[must_use]
    pub fn remove_user(&self, username: &str) -> bool {
        self.users.write().remove(username).is_some()
    }

    #[must_use]
    pub fn user_count(&self) -> usize {
        self.users.read().len()
    }

    #[must_use]
    pub fn has_user(&self, username: &str) -> bool {
        self.users.read().contains_key(username)
    }

    #[must_use]
    pub fn list_users(&self) -> Vec<String> {
        self.users.read().keys().cloned().collect()
    }

    #[must_use]
    pub fn verify_user_password(&self, username: &str, password: &str) -> bool {
        let users = self.users.read();
        users
            .get(username)
            .is_some_and(|hash| Self::verify_password(password, hash))
    }

    #[must_use]
    pub fn verify_user_password_blocking(&self, username: &str, password: &str) -> bool {
        let users = self.users.read();
        users
            .get(username)
            .is_some_and(|hash| Self::verify_password(password, hash))
    }

    /// Generates an Argon2 hash for a password
    ///
    /// # Errors
    ///
    /// Returns an error if Argon2 hashing fails
    pub fn hash_password(password: &str) -> Result<String> {
        let mut bytes = [0u8; Salt::RECOMMENDED_LENGTH];
        getrandom::fill(&mut bytes).map_err(|e| {
            error!("Failed to generate random salt: {e}");
            MqttError::AuthenticationFailed
        })?;
        let salt = SaltString::encode_b64(&bytes).map_err(|e| {
            error!("Failed to encode salt: {e}");
            MqttError::AuthenticationFailed
        })?;
        let argon2 = Argon2::default();
        argon2
            .hash_password(password.as_bytes(), &salt)
            .map(|hash| hash.to_string())
            .map_err(|e| {
                error!("Failed to hash password: {e}");
                MqttError::AuthenticationFailed
            })
    }

    #[must_use]
    pub fn verify_password(password: &str, hash: &str) -> bool {
        let Ok(parsed_hash) = PasswordHash::new(hash).map_err(|e| {
            error!("Failed to parse password hash: {e}");
        }) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed_hash)
            .is_ok()
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
            let Some(username) = &connect.username else {
                if self.allow_anonymous {
                    debug!("Anonymous connection allowed");
                    return Ok(AuthResult::success());
                }
                return Ok(AuthResult::fail(ReasonCode::BadUsernameOrPassword));
            };

            let Some(password) = &connect.password else {
                if self.allow_anonymous {
                    debug!("Anonymous connection allowed (username without password)");
                    return Ok(AuthResult::success());
                }
                return Ok(AuthResult::fail(ReasonCode::BadUsernameOrPassword));
            };

            let users = self.users.read();
            if let Some(password_hash) = users.get(username) {
                let password_str = String::from_utf8_lossy(password);

                if Self::verify_password(&password_str, password_hash) {
                    debug!("Authentication successful for user: {username}");
                    Ok(AuthResult::success_with_user(username.clone()))
                } else {
                    warn!("Authentication failed for user: {username} (wrong password)");
                    Ok(AuthResult::fail(ReasonCode::BadUsernameOrPassword))
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
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }
}

/// Comprehensive authentication provider with password auth and ACL support
#[derive(Debug)]
pub struct ComprehensiveAuthProvider {
    password_provider: PasswordAuthProvider,
    acl_manager: AclManager,
}

impl ComprehensiveAuthProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            password_provider: PasswordAuthProvider::new(),
            acl_manager: AclManager::new(),
        }
    }

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

    #[must_use]
    pub fn password_provider(&self) -> &PasswordAuthProvider {
        &self.password_provider
    }

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
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        let client_id = client_id.to_string();
        Box::pin(async move {
            debug!(
                "Checking publish authorization for client={}, user={:?}, topic={}",
                client_id, user_id, topic
            );
            let allowed = self.acl_manager.check_publish(user_id, topic).await;
            debug!("Authorization result: {}", allowed);
            allowed
        })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        user_id: Option<&'a str>,
        topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            self.acl_manager
                .check_subscribe(user_id, topic_filter)
                .await
        })
    }
}

/// Certificate-based authentication provider using X.509 client certificates
#[derive(Debug)]
pub struct CertificateAuthProvider {
    allowed_certs: Arc<RwLock<HashMap<String, String>>>,
    #[cfg(not(target_arch = "wasm32"))]
    cert_file: Option<std::path::PathBuf>,
}

impl CertificateAuthProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            allowed_certs: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(not(target_arch = "wasm32"))]
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

            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let parts: Vec<&str> = line.splitn(2, ':').collect();
            if parts.len() != 2 {
                warn!("Invalid format in certificate file at line {line_num}");
                continue;
            }

            let fingerprint = parts[0].trim().to_lowercase();
            let username = parts[1].trim().to_string();

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

        *self.allowed_certs.write() = certs;

        info!(
            "Loaded {} certificate mappings from file: {}",
            self.allowed_certs.read().len(),
            path.display()
        );
        Ok(())
    }

    pub fn add_certificate(&self, fingerprint: &str, username: &str) {
        let normalized_fingerprint = fingerprint.to_lowercase();
        self.allowed_certs
            .write()
            .insert(normalized_fingerprint, username.to_string());
    }

    #[must_use]
    pub fn remove_certificate(&self, fingerprint: &str) -> bool {
        let normalized_fingerprint = fingerprint.to_lowercase();
        self.allowed_certs
            .write()
            .remove(&normalized_fingerprint)
            .is_some()
    }

    #[must_use]
    pub fn cert_count(&self) -> usize {
        self.allowed_certs.read().len()
    }

    #[must_use]
    pub fn has_certificate(&self, fingerprint: &str) -> bool {
        let normalized_fingerprint = fingerprint.to_lowercase();
        self.allowed_certs
            .read()
            .contains_key(&normalized_fingerprint)
    }

    #[must_use]
    pub fn calculate_fingerprint(cert_der: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(cert_der);
        hex::encode(hasher.finalize())
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
            debug!(
                "Certificate authentication attempted for client: {} from {}",
                connect.client_id, client_addr
            );

            if connect.client_id.starts_with("cert:") {
                let fingerprint = &connect.client_id[5..];

                if fingerprint.len() != 64 || !fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
                    warn!("Certificate authentication failed: malformed fingerprint in client_id");
                    return Ok(AuthResult::fail(ReasonCode::ClientIdentifierNotValid));
                }

                let certs = self.allowed_certs.read();
                if let Some(username) = certs.get(&fingerprint.to_lowercase()) {
                    debug!(
                        fingerprint = fingerprint,
                        username = username.as_str(),
                        "Certificate authentication succeeded"
                    );
                    return Ok(AuthResult::success_with_user(username.clone()));
                }
                warn!("Certificate authentication failed: unknown fingerprint");
                return Ok(AuthResult::fail(ReasonCode::NotAuthorized));
            }

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
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        _user_id: Option<&'a str>,
        _topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { true })
    }
}
