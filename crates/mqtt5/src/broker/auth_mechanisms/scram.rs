use crate::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use crate::error::Result;
use crate::packet::connect::ConnectPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use base64::prelude::*;
use ring::{hmac, pbkdf2};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, warn};

const SCRAM_ITERATION_COUNT: u32 = 310_000;
const SCRAM_SALT_LENGTH: usize = 16;
const SCRAM_STATE_TTL: Duration = Duration::from_secs(60);
const DEFAULT_MAX_CONCURRENT_AUTH: usize = 1000;

#[derive(Clone)]
pub struct ScramCredentials {
    pub salt: Vec<u8>,
    pub iteration_count: u32,
    pub stored_key: [u8; 32],
    pub server_key: [u8; 32],
}

impl ScramCredentials {
    /// Creates SCRAM credentials from a password using default iteration count.
    ///
    /// # Errors
    /// Returns an error if random salt generation fails.
    pub fn from_password(password: &str) -> std::result::Result<Self, getrandom::Error> {
        Self::from_password_with_iterations(password, SCRAM_ITERATION_COUNT)
    }

    /// Creates SCRAM credentials from a password with custom iteration count.
    ///
    /// # Errors
    /// Returns an error if random salt generation fails.
    pub fn from_password_with_iterations(
        password: &str,
        iterations: u32,
    ) -> std::result::Result<Self, getrandom::Error> {
        let mut salt = vec![0u8; SCRAM_SALT_LENGTH];
        getrandom::fill(&mut salt)?;

        Ok(Self::from_password_and_salt(password, &salt, iterations))
    }

    #[must_use]
    pub fn from_password_and_salt(password: &str, salt: &[u8], iterations: u32) -> Self {
        let salted_password = pbkdf2_sha256(password.as_bytes(), salt, iterations);
        let client_key = hmac_sha256(&salted_password, b"Client Key");
        let stored_key = sha256(&client_key);
        let server_key = hmac_sha256(&salted_password, b"Server Key");

        Self {
            salt: salt.to_vec(),
            iteration_count: iterations,
            stored_key,
            server_key,
        }
    }
}

pub trait ScramCredentialStore: Send + Sync {
    fn get_credentials(&self, username: &str) -> Option<ScramCredentials>;
}

pub struct FileBasedScramCredentialStore {
    credentials: RwLock<HashMap<String, ScramCredentials>>,
}

impl FileBasedScramCredentialStore {
    #[must_use]
    pub fn new() -> Self {
        Self {
            credentials: RwLock::new(HashMap::new()),
        }
    }

    /// # Errors
    /// Returns an error if the file cannot be read or parsed.
    pub fn load_from_file(path: &std::path::Path) -> std::io::Result<Self> {
        use std::io::{BufRead, BufReader};
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        let mut credentials = HashMap::new();

        for (line_num, line) in reader.lines().enumerate() {
            let line = line?;
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            match Self::parse_credential_line(line) {
                Ok((username, creds)) => {
                    credentials.insert(username, creds);
                }
                Err(e) => {
                    warn!(line = line_num + 1, error = %e, "failed to parse SCRAM credential line");
                }
            }
        }

        debug!(
            count = credentials.len(),
            "loaded SCRAM credentials from file"
        );
        Ok(Self {
            credentials: RwLock::new(credentials),
        })
    }

    fn parse_credential_line(
        line: &str,
    ) -> std::result::Result<(String, ScramCredentials), &'static str> {
        let parts: Vec<&str> = line.split(':').collect();
        if parts.len() != 5 {
            return Err("expected 5 colon-separated fields");
        }

        let username = parts[0].to_string();
        if username.is_empty() {
            return Err("username cannot be empty");
        }

        let salt = BASE64_STANDARD
            .decode(parts[1])
            .map_err(|_| "invalid base64 salt")?;
        let iteration_count: u32 = parts[2].parse().map_err(|_| "invalid iteration count")?;

        let stored_key_vec = BASE64_STANDARD
            .decode(parts[3])
            .map_err(|_| "invalid base64 stored_key")?;
        if stored_key_vec.len() != 32 {
            return Err("stored_key must be 32 bytes");
        }
        let mut stored_key = [0u8; 32];
        stored_key.copy_from_slice(&stored_key_vec);

        let server_key_vec = BASE64_STANDARD
            .decode(parts[4])
            .map_err(|_| "invalid base64 server_key")?;
        if server_key_vec.len() != 32 {
            return Err("server_key must be 32 bytes");
        }
        let mut server_key = [0u8; 32];
        server_key.copy_from_slice(&server_key_vec);

        Ok((
            username,
            ScramCredentials {
                salt,
                iteration_count,
                stored_key,
                server_key,
            },
        ))
    }
}

impl Default for FileBasedScramCredentialStore {
    fn default() -> Self {
        Self::new()
    }
}

impl ScramCredentialStore for FileBasedScramCredentialStore {
    fn get_credentials(&self, username: &str) -> Option<ScramCredentials> {
        self.credentials.read().unwrap().get(username).cloned()
    }
}

/// Generates a SCRAM credential line for storage in a credentials file.
///
/// # Errors
/// Returns an error if random salt generation fails.
pub fn generate_scram_credential_line(
    username: &str,
    password: &str,
) -> std::result::Result<String, getrandom::Error> {
    let creds = ScramCredentials::from_password(password)?;
    Ok(format!(
        "{}:{}:{}:{}:{}",
        username,
        BASE64_STANDARD.encode(&creds.salt),
        creds.iteration_count,
        BASE64_STANDARD.encode(creds.stored_key),
        BASE64_STANDARD.encode(creds.server_key),
    ))
}

/// Generates a SCRAM credential line with custom iteration count.
///
/// # Errors
/// Returns an error if random salt generation fails.
pub fn generate_scram_credential_line_with_iterations(
    username: &str,
    password: &str,
    iterations: u32,
) -> std::result::Result<String, getrandom::Error> {
    let creds = ScramCredentials::from_password_with_iterations(password, iterations)?;
    Ok(format!(
        "{}:{}:{}:{}:{}",
        username,
        BASE64_STANDARD.encode(&creds.salt),
        creds.iteration_count,
        BASE64_STANDARD.encode(creds.stored_key),
        BASE64_STANDARD.encode(creds.server_key),
    ))
}

struct ScramServerState {
    username: String,
    client_first_bare: String,
    server_first: String,
    server_nonce: String,
    credentials: ScramCredentials,
    created_at: Instant,
}

pub struct ScramSha256AuthProvider<S: ScramCredentialStore> {
    store: Arc<S>,
    state: TokioRwLock<HashMap<String, ScramServerState>>,
    max_states: usize,
}

impl<S: ScramCredentialStore> ScramSha256AuthProvider<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self {
            store,
            state: TokioRwLock::new(HashMap::new()),
            max_states: DEFAULT_MAX_CONCURRENT_AUTH,
        }
    }

    #[must_use]
    pub fn with_max_concurrent_auth(mut self, max_states: usize) -> Self {
        self.max_states = max_states;
        self
    }

    async fn cleanup_expired_states(&self) {
        let now = Instant::now();
        self.state
            .write()
            .await
            .retain(|_, state| now.duration_since(state.created_at) < SCRAM_STATE_TTL);
    }
}

impl<S: ScramCredentialStore + 'static> AuthProvider for ScramSha256AuthProvider<S> {
    fn authenticate<'a>(
        &'a self,
        _connect: &'a ConnectPacket,
        _client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move { Ok(AuthResult::fail(ReasonCode::BadAuthenticationMethod)) })
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
        let cid = client_id.to_string();

        Box::pin(async move {
            self.cleanup_expired_states().await;

            if method != "SCRAM-SHA-256" {
                debug!(method = %method, "SCRAM auth provider received non-SCRAM method");
                return Ok(EnhancedAuthResult::fail(
                    method,
                    ReasonCode::BadAuthenticationMethod,
                ));
            }

            let Some(data) = auth_data else {
                warn!("SCRAM authentication failed: no data provided");
                return Ok(EnhancedAuthResult::fail_with_reason(
                    method,
                    ReasonCode::NotAuthorized,
                    "No authentication data provided".to_string(),
                ));
            };

            let msg = String::from_utf8_lossy(data);

            let has_state = self.state.read().await.contains_key(&cid);

            if has_state {
                Ok(self.handle_client_final(&cid, &method, &msg).await)
            } else {
                Ok(self.handle_client_first(&cid, &method, &msg).await)
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

impl<S: ScramCredentialStore> ScramSha256AuthProvider<S> {
    async fn handle_client_first(
        &self,
        client_id: &str,
        method: &str,
        message: &str,
    ) -> EnhancedAuthResult {
        let client_first_bare = if let Some(stripped) = message.strip_prefix("n,,") {
            stripped
        } else if message.starts_with("y,,") || message.starts_with("p=") {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Channel binding not supported".to_string(),
            );
        } else {
            message
        };

        let attrs = parse_scram_attributes(client_first_bare);

        let Some(username) = attrs.get("n") else {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Missing username in client-first message".to_string(),
            );
        };

        let Some(client_nonce) = attrs.get("r") else {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Missing nonce in client-first message".to_string(),
            );
        };

        let Some(credentials) = self.store.get_credentials(username) else {
            warn!("SCRAM authentication failed: invalid credentials");
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Authentication failed".to_string(),
            );
        };

        let mut server_nonce_bytes = [0u8; 24];
        if let Err(e) = getrandom::fill(&mut server_nonce_bytes) {
            warn!(error = %e, "failed to generate random nonce");
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::UnspecifiedError,
                "Internal error".to_string(),
            );
        }
        let server_nonce = BASE64_STANDARD.encode(server_nonce_bytes);
        let combined_nonce = format!("{client_nonce}{server_nonce}");

        let salt_b64 = BASE64_STANDARD.encode(&credentials.salt);
        let server_first = format!(
            "r={},s={},i={}",
            combined_nonce, salt_b64, credentials.iteration_count
        );

        let state = ScramServerState {
            username: username.clone(),
            client_first_bare: client_first_bare.to_string(),
            server_first: server_first.clone(),
            server_nonce: combined_nonce,
            credentials,
            created_at: Instant::now(),
        };

        {
            let mut states = self.state.write().await;
            if states.len() >= self.max_states {
                warn!(
                    max_states = self.max_states,
                    "SCRAM authentication rejected: too many concurrent authentications"
                );
                return EnhancedAuthResult::fail_with_reason(
                    method.to_string(),
                    ReasonCode::QuotaExceeded,
                    "Too many concurrent authentications".to_string(),
                );
            }
            if states.contains_key(client_id) {
                warn!(
                    client_id = %client_id,
                    "SCRAM authentication rejected: concurrent authentication in progress"
                );
                return EnhancedAuthResult::fail_with_reason(
                    method.to_string(),
                    ReasonCode::NotAuthorized,
                    "Authentication already in progress for this client".to_string(),
                );
            }
            states.insert(client_id.to_string(), state);
        }

        debug!(username = %username, "SCRAM client-first processed, sending server-first");
        EnhancedAuthResult::continue_auth(method.to_string(), Some(server_first.into_bytes()))
    }

    async fn handle_client_final(
        &self,
        client_id: &str,
        method: &str,
        message: &str,
    ) -> EnhancedAuthResult {
        let Some(state) = self.state.write().await.remove(client_id) else {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "SCRAM state not found".to_string(),
            );
        };

        let attrs = parse_scram_attributes(message);

        let Some(channel_binding) = attrs.get("c") else {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Missing channel binding in client-final".to_string(),
            );
        };

        if channel_binding != "biws" {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Invalid channel binding".to_string(),
            );
        }

        let Some(nonce) = attrs.get("r") else {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Missing nonce in client-final".to_string(),
            );
        };

        if nonce != &state.server_nonce {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Nonce mismatch".to_string(),
            );
        }

        let Some(client_proof_b64) = attrs.get("p") else {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Missing proof in client-final".to_string(),
            );
        };

        let Ok(client_proof) = BASE64_STANDARD.decode(client_proof_b64) else {
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Invalid proof encoding".to_string(),
            );
        };

        let client_final_without_proof = format!("c={channel_binding},r={nonce}");
        let auth_message = format!(
            "{},{},{}",
            state.client_first_bare, state.server_first, client_final_without_proof
        );

        let client_signature = hmac_sha256(&state.credentials.stored_key, auth_message.as_bytes());
        let client_key = xor_bytes(&client_proof, &client_signature);
        let computed_stored_key = sha256(&client_key);

        if !constant_time_compare(&computed_stored_key, &state.credentials.stored_key) {
            warn!("SCRAM authentication failed: invalid credentials");
            return EnhancedAuthResult::fail_with_reason(
                method.to_string(),
                ReasonCode::NotAuthorized,
                "Authentication failed".to_string(),
            );
        }

        let server_signature = hmac_sha256(&state.credentials.server_key, auth_message.as_bytes());
        let server_final = format!("v={}", BASE64_STANDARD.encode(server_signature));

        debug!(username = %state.username, "SCRAM authentication successful");

        let mut result = EnhancedAuthResult::success_with_user(method.to_string(), state.username);
        result.auth_data = Some(server_final.into_bytes());
        result
    }
}

fn parse_scram_attributes(message: &str) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    for part in message.split(',') {
        if let Some((key, value)) = part.split_once('=') {
            attrs.insert(key.to_string(), value.to_string());
        }
    }
    attrs
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(iterations).expect("iterations must be non-zero"),
        salt,
        password,
        &mut out,
    );
    out
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&key, data);
    let mut result = [0u8; 32];
    result.copy_from_slice(tag.as_ref());
    result
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

fn xor_bytes(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect()
}

fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestCredentialStore {
        credentials: RwLock<HashMap<String, ScramCredentials>>,
    }

    impl TestCredentialStore {
        fn new() -> Self {
            Self {
                credentials: RwLock::new(HashMap::new()),
            }
        }

        fn add_user(&self, username: &str, password: &str) {
            let creds = ScramCredentials::from_password(password).unwrap();
            self.credentials
                .write()
                .unwrap()
                .insert(username.to_string(), creds);
        }

        fn add_user_with_salt(&self, username: &str, password: &str, salt: &[u8], iterations: u32) {
            let creds = ScramCredentials::from_password_and_salt(password, salt, iterations);
            self.credentials
                .write()
                .unwrap()
                .insert(username.to_string(), creds);
        }
    }

    impl ScramCredentialStore for TestCredentialStore {
        fn get_credentials(&self, username: &str) -> Option<ScramCredentials> {
            self.credentials.read().unwrap().get(username).cloned()
        }
    }

    #[test]
    fn test_scram_credentials_from_password() {
        let creds = ScramCredentials::from_password("testpassword").unwrap();
        assert_eq!(creds.salt.len(), SCRAM_SALT_LENGTH);
        assert_eq!(creds.iteration_count, SCRAM_ITERATION_COUNT);
        assert_eq!(creds.stored_key.len(), 32);
        assert_eq!(creds.server_key.len(), 32);
    }

    #[test]
    fn test_scram_credentials_deterministic() {
        let salt = b"fixed_salt_value";
        let creds1 = ScramCredentials::from_password_and_salt("password", salt, 4096);
        let creds2 = ScramCredentials::from_password_and_salt("password", salt, 4096);
        assert_eq!(creds1.stored_key, creds2.stored_key);
        assert_eq!(creds1.server_key, creds2.server_key);
    }

    #[test]
    fn test_parse_scram_attributes() {
        let msg = "n=user,r=fyko+d2lbbFgONRv9qkxdawL";
        let attrs = parse_scram_attributes(msg);
        assert_eq!(attrs.get("n"), Some(&"user".to_string()));
        assert_eq!(
            attrs.get("r"),
            Some(&"fyko+d2lbbFgONRv9qkxdawL".to_string())
        );
    }

    #[tokio::test]
    async fn test_scram_wrong_method() {
        let store = Arc::new(TestCredentialStore::new());
        let provider = ScramSha256AuthProvider::new(store);

        let result = provider
            .authenticate_enhanced("PLAIN", None, "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Failed
        );
        assert_eq!(result.reason_code, ReasonCode::BadAuthenticationMethod);
    }

    #[tokio::test]
    async fn test_scram_unknown_user() {
        let store = Arc::new(TestCredentialStore::new());
        let provider = ScramSha256AuthProvider::new(store);

        let client_first = "n,,n=unknownuser,r=clientnonce123";
        let result = provider
            .authenticate_enhanced("SCRAM-SHA-256", Some(client_first.as_bytes()), "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Failed
        );
    }

    #[tokio::test]
    async fn test_scram_client_first_returns_continue() {
        let store = Arc::new(TestCredentialStore::new());
        store.add_user("testuser", "testpass");
        let provider = ScramSha256AuthProvider::new(store);

        let client_first = "n,,n=testuser,r=clientnonce123";
        let result = provider
            .authenticate_enhanced("SCRAM-SHA-256", Some(client_first.as_bytes()), "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Continue
        );
        assert!(result.auth_data.is_some());

        let server_first = String::from_utf8_lossy(result.auth_data.as_ref().unwrap());
        assert!(server_first.starts_with("r=clientnonce123"));
        assert!(server_first.contains(",s="));
        assert!(server_first.contains(",i="));
    }

    #[tokio::test]
    async fn test_scram_full_handshake() {
        let salt = b"test_salt_12345!";
        let password = "testpassword";

        let store = Arc::new(TestCredentialStore::new());
        store.add_user_with_salt("testuser", password, salt, 4096);
        let provider = ScramSha256AuthProvider::new(store);

        let client_nonce = "rOprNGfwEbeRWgbNEkqO";
        let client_first = format!("n,,n=testuser,r={client_nonce}");

        let result = provider
            .authenticate_enhanced("SCRAM-SHA-256", Some(client_first.as_bytes()), "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Continue
        );

        let server_first = String::from_utf8_lossy(result.auth_data.as_ref().unwrap());
        let server_attrs = parse_scram_attributes(&server_first);
        let combined_nonce = server_attrs.get("r").unwrap();
        let salt_b64 = server_attrs.get("s").unwrap();
        let iterations: u32 = server_attrs.get("i").unwrap().parse().unwrap();

        let salt_decoded = BASE64_STANDARD.decode(salt_b64).unwrap();
        let salted_password = pbkdf2_sha256(password.as_bytes(), &salt_decoded, iterations);
        let client_key = hmac_sha256(&salted_password, b"Client Key");
        let stored_key = sha256(&client_key);

        let client_first_bare = format!("n=testuser,r={client_nonce}");
        let client_final_without_proof = format!("c=biws,r={combined_nonce}");
        let auth_message =
            format!("{client_first_bare},{server_first},{client_final_without_proof}");

        let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
        let client_proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(a, b)| a ^ b)
            .collect();

        let proof_b64 = BASE64_STANDARD.encode(&client_proof);
        let client_final = format!("c=biws,r={combined_nonce},p={proof_b64}");

        let result = provider
            .authenticate_enhanced("SCRAM-SHA-256", Some(client_final.as_bytes()), "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Success,
            "Expected success, got: {result:?}"
        );

        let server_final = String::from_utf8_lossy(result.auth_data.as_ref().unwrap());
        assert!(server_final.starts_with("v="));
    }

    #[test]
    fn test_generate_scram_credential_line() {
        let line = generate_scram_credential_line("alice", "password123").unwrap();
        let parts: Vec<&str> = line.split(':').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], "alice");
        assert!(BASE64_STANDARD.decode(parts[1]).is_ok());
        assert_eq!(parts[2], "310000");
        assert!(BASE64_STANDARD.decode(parts[3]).is_ok());
        assert!(BASE64_STANDARD.decode(parts[4]).is_ok());
    }

    #[test]
    fn test_file_based_store_parse_line() {
        let line = generate_scram_credential_line("bob", "secret").unwrap();
        let (username, creds) =
            FileBasedScramCredentialStore::parse_credential_line(&line).unwrap();
        assert_eq!(username, "bob");
        assert_eq!(creds.iteration_count, SCRAM_ITERATION_COUNT);
        assert_eq!(creds.salt.len(), SCRAM_SALT_LENGTH);
    }

    #[test]
    fn test_file_based_store_load() {
        use std::io::Write;
        let dir = std::env::temp_dir();
        let path = dir.join("test_scram_creds.txt");

        let line1 = generate_scram_credential_line("user1", "pass1").unwrap();
        let line2 = generate_scram_credential_line("user2", "pass2").unwrap();

        let mut file = std::fs::File::create(&path).unwrap();
        writeln!(file, "{line1}").unwrap();
        writeln!(file, "# comment line\n").unwrap();
        writeln!(file, "{line2}").unwrap();

        let store = FileBasedScramCredentialStore::load_from_file(&path).unwrap();
        assert!(store.get_credentials("user1").is_some());
        assert!(store.get_credentials("user2").is_some());
        assert!(store.get_credentials("user3").is_none());

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn test_file_based_store_invalid_lines() {
        let result = FileBasedScramCredentialStore::parse_credential_line("invalid");
        assert!(result.is_err());

        let result = FileBasedScramCredentialStore::parse_credential_line("a:b:c");
        assert!(result.is_err());

        let result = FileBasedScramCredentialStore::parse_credential_line(":salt:100:key:key");
        assert!(result.is_err());
    }
}
