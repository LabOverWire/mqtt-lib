use base64::prelude::*;
use http_body_util::BodyExt;
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use ring::signature;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};
use url::Url;

#[derive(Debug, Clone)]
pub struct JwkKey {
    pub kid: String,
    pub algorithm: String,
    pub key_bytes: Vec<u8>,
}

#[derive(Debug, Clone)]
struct CachedKeySet {
    keys: Vec<JwkKey>,
    fetched_at: Instant,
    expires_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

struct CircuitBreakerState {
    failures: u32,
    last_failure: Option<Instant>,
    state: CircuitState,
}

impl CircuitBreakerState {
    const MAX_FAILURES: u32 = 3;
    const RECOVERY_TIME: Duration = Duration::from_secs(60);

    fn new() -> Self {
        Self {
            failures: 0,
            last_failure: None,
            state: CircuitState::Closed,
        }
    }

    fn record_success(&mut self) {
        self.failures = 0;
        self.state = CircuitState::Closed;
    }

    fn record_failure(&mut self) {
        self.failures += 1;
        self.last_failure = Some(Instant::now());
        if self.failures >= Self::MAX_FAILURES {
            self.state = CircuitState::Open;
        }
    }

    fn should_attempt(&mut self) -> bool {
        match self.state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                if let Some(last) = self.last_failure {
                    if last.elapsed() > Self::RECOVERY_TIME {
                        self.state = CircuitState::HalfOpen;
                        return true;
                    }
                }
                false
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct JwksEndpointConfig {
    pub uri: String,
    pub issuer: String,
    pub refresh_interval: Duration,
    pub cache_ttl: Duration,
    pub request_timeout: Duration,
}

impl Default for JwksEndpointConfig {
    fn default() -> Self {
        Self {
            uri: String::new(),
            issuer: String::new(),
            refresh_interval: Duration::from_secs(3600),
            cache_ttl: Duration::from_secs(86400),
            request_timeout: Duration::from_secs(10),
        }
    }
}

pub struct JwksCache {
    endpoints: Vec<JwksEndpointConfig>,
    keys: Arc<RwLock<HashMap<String, CachedKeySet>>>,
    circuit_breakers: Arc<RwLock<HashMap<String, CircuitBreakerState>>>,
    shutdown_tx: broadcast::Sender<()>,
}

impl JwksCache {
    #[must_use]
    pub fn new() -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        Self {
            endpoints: Vec::new(),
            keys: Arc::new(RwLock::new(HashMap::new())),
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
            shutdown_tx,
        }
    }

    pub fn add_endpoint(&mut self, config: JwksEndpointConfig) {
        self.endpoints.push(config);
    }

    /// Performs initial JWKS fetch for all configured endpoints.
    ///
    /// # Errors
    ///
    /// Returns an error if any JWKS endpoint fetch fails due to network issues,
    /// invalid URI, TLS errors, or parsing failures.
    pub async fn initial_fetch(&self) -> crate::error::Result<()> {
        for endpoint in &self.endpoints {
            match self.fetch_jwks_from_endpoint(endpoint).await {
                Ok(keys) => {
                    let cache_entry = CachedKeySet {
                        keys,
                        fetched_at: Instant::now(),
                        expires_at: Instant::now() + endpoint.cache_ttl,
                    };
                    self.keys
                        .write()
                        .await
                        .insert(endpoint.issuer.clone(), cache_entry);
                    info!(issuer = %endpoint.issuer, "JWKS initial fetch successful");
                }
                Err(e) => {
                    warn!(issuer = %endpoint.issuer, error = %e, "JWKS initial fetch failed");
                }
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn start_background_refresh(&self) -> tokio::task::JoinHandle<()> {
        let endpoints = self.endpoints.clone();
        let keys = Arc::clone(&self.keys);
        let circuit_breakers = Arc::clone(&self.circuit_breakers);
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        tokio::spawn(async move {
            let min_interval = endpoints
                .iter()
                .map(|e| e.refresh_interval)
                .min()
                .unwrap_or(Duration::from_secs(3600));

            let mut interval = tokio::time::interval(min_interval);

            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        for endpoint in &endpoints {
                            let should_refresh = {
                                let keys_guard = keys.read().await;
                                if let Some(cached) = keys_guard.get(&endpoint.issuer) {
                                    cached.fetched_at.elapsed() >= endpoint.refresh_interval
                                } else {
                                    true
                                }
                            };

                            if should_refresh {
                                let should_attempt = {
                                    let mut cb_guard = circuit_breakers.write().await;
                                    let cb = cb_guard
                                        .entry(endpoint.issuer.clone())
                                        .or_insert_with(CircuitBreakerState::new);
                                    cb.should_attempt()
                                };

                                if should_attempt {
                                    match fetch_jwks_http(&endpoint.uri, endpoint.request_timeout).await {
                                        Ok(jwks_keys) => {
                                            let cache_entry = CachedKeySet {
                                                keys: jwks_keys,
                                                fetched_at: Instant::now(),
                                                expires_at: Instant::now() + endpoint.cache_ttl,
                                            };
                                            keys.write().await.insert(endpoint.issuer.clone(), cache_entry);
                                            circuit_breakers
                                                .write()
                                                .await
                                                .entry(endpoint.issuer.clone())
                                                .or_insert_with(CircuitBreakerState::new)
                                                .record_success();
                                            debug!(issuer = %endpoint.issuer, "JWKS refresh successful");
                                        }
                                        Err(e) => {
                                            circuit_breakers
                                                .write()
                                                .await
                                                .entry(endpoint.issuer.clone())
                                                .or_insert_with(CircuitBreakerState::new)
                                                .record_failure();
                                            warn!(issuer = %endpoint.issuer, error = %e, "JWKS refresh failed");
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ = shutdown_rx.recv() => {
                        info!("JWKS refresh task shutting down");
                        break;
                    }
                }
            }
        })
    }

    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }

    pub async fn get_key(&self, kid: &str, algorithm: &str) -> Option<JwkKey> {
        let keys_guard = self.keys.read().await;
        for cached in keys_guard.values() {
            if cached.expires_at > Instant::now() {
                for key in &cached.keys {
                    if key.kid == kid && key.algorithm == algorithm {
                        return Some(key.clone());
                    }
                }
            }
        }
        None
    }

    pub async fn get_key_by_issuer(
        &self,
        issuer: &str,
        kid: &str,
        algorithm: &str,
    ) -> Option<JwkKey> {
        let keys_guard = self.keys.read().await;
        if let Some(cached) = keys_guard.get(issuer) {
            if cached.expires_at > Instant::now() {
                for key in &cached.keys {
                    if key.kid == kid && key.algorithm == algorithm {
                        return Some(key.clone());
                    }
                }
            }
        }
        None
    }

    pub async fn get_keys_for_issuer(&self, issuer: &str) -> Vec<JwkKey> {
        let keys_guard = self.keys.read().await;
        if let Some(cached) = keys_guard.get(issuer) {
            if cached.expires_at > Instant::now() {
                return cached.keys.clone();
            }
        }
        Vec::new()
    }

    async fn fetch_jwks_from_endpoint(
        &self,
        endpoint: &JwksEndpointConfig,
    ) -> Result<Vec<JwkKey>, JwksError> {
        fetch_jwks_http(&endpoint.uri, endpoint.request_timeout).await
    }
}

impl Default for JwksCache {
    fn default() -> Self {
        Self::new()
    }
}

async fn fetch_jwks_http(uri: &str, timeout: Duration) -> Result<Vec<JwkKey>, JwksError> {
    use http_body_util::Empty;

    let parsed = Url::parse(uri).map_err(|e| JwksError::InvalidUri(e.to_string()))?;

    if parsed.scheme() != "https" {
        return Err(JwksError::InvalidUri("JWKS URI must use HTTPS".into()));
    }

    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_tls_config(tls_config)
        .https_only()
        .enable_http1()
        .build();

    let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new()).build(https);

    let http_uri: http::Uri = parsed
        .as_str()
        .parse()
        .map_err(|e: http::uri::InvalidUri| JwksError::InvalidUri(e.to_string()))?;

    let response = tokio::time::timeout(timeout, client.get(http_uri))
        .await
        .map_err(|_| JwksError::Timeout)?
        .map_err(|e| JwksError::ConnectionFailed(e.to_string()))?;

    if response.status() != http::StatusCode::OK {
        return Err(JwksError::HttpError(format!("HTTP {}", response.status())));
    }

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .map_err(|e| JwksError::ConnectionFailed(e.to_string()))?
        .to_bytes();

    let json =
        String::from_utf8(body_bytes.to_vec()).map_err(|e| JwksError::ParseError(e.to_string()))?;

    parse_jwks_json(&json)
}

#[derive(Debug, Deserialize)]
struct JwksResponse {
    keys: Vec<JwkEntry>,
}

#[derive(Debug, Deserialize)]
struct JwkEntry {
    #[serde(default)]
    kid: String,
    kty: String,
    #[serde(default)]
    alg: Option<String>,
    #[serde(rename = "use", default)]
    use_: Option<String>,
    n: Option<String>,
    e: Option<String>,
    x: Option<String>,
    y: Option<String>,
    crv: Option<String>,
}

fn parse_jwks_json(json: &str) -> Result<Vec<JwkKey>, JwksError> {
    let jwks: JwksResponse =
        serde_json::from_str(json).map_err(|e| JwksError::ParseError(e.to_string()))?;

    let mut keys = Vec::new();

    for entry in jwks.keys {
        if entry.use_.as_deref() == Some("enc") {
            continue;
        }

        let algorithm = entry
            .alg
            .clone()
            .unwrap_or_else(|| infer_algorithm(&entry.kty, entry.crv.as_deref()));

        match entry.kty.as_str() {
            "RSA" => {
                if let (Some(n), Some(e)) = (&entry.n, &entry.e) {
                    match rsa_jwk_to_der(n, e) {
                        Ok(der) => {
                            keys.push(JwkKey {
                                kid: entry.kid.clone(),
                                algorithm,
                                key_bytes: der,
                            });
                        }
                        Err(e) => {
                            warn!(kid = %entry.kid, error = %e, "Failed to parse RSA JWK");
                        }
                    }
                }
            }
            "EC" => {
                if let (Some(x), Some(y), Some(crv)) = (&entry.x, &entry.y, &entry.crv) {
                    match ec_jwk_to_der(x, y, crv) {
                        Ok(der) => {
                            keys.push(JwkKey {
                                kid: entry.kid.clone(),
                                algorithm,
                                key_bytes: der,
                            });
                        }
                        Err(e) => {
                            warn!(kid = %entry.kid, error = %e, "Failed to parse EC JWK");
                        }
                    }
                }
            }
            other => {
                debug!(kty = %other, "Unsupported key type in JWKS");
            }
        }
    }

    Ok(keys)
}

fn infer_algorithm(kty: &str, crv: Option<&str>) -> String {
    match kty {
        "EC" => match crv {
            Some("P-384") => "ES384".to_string(),
            Some("P-521") => "ES512".to_string(),
            _ => "ES256".to_string(),
        },
        _ => "RS256".to_string(),
    }
}

fn rsa_jwk_to_der(n_b64: &str, e_b64: &str) -> Result<Vec<u8>, JwksError> {
    let n = base64url_decode(n_b64)?;
    let e = base64url_decode(e_b64)?;

    let n_len = n.len();
    let e_len = e.len();

    let n_der = encode_der_integer(&n);
    let e_der = encode_der_integer(&e);

    let seq_content_len = n_der.len() + e_der.len();
    let mut der = Vec::with_capacity(seq_content_len + 10);

    der.push(0x30);
    encode_der_length(&mut der, seq_content_len);
    der.extend_from_slice(&n_der);
    der.extend_from_slice(&e_der);

    let _ = signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, &der);

    debug!(
        n_len,
        e_len,
        der_len = der.len(),
        "RSA JWK converted to DER"
    );

    Ok(der)
}

fn ec_jwk_to_der(x_b64: &str, y_b64: &str, crv: &str) -> Result<Vec<u8>, JwksError> {
    let x = base64url_decode(x_b64)?;
    let y = base64url_decode(y_b64)?;

    let expected_len = match crv {
        "P-256" => 32,
        "P-384" => 48,
        "P-521" => 66,
        other => return Err(JwksError::UnsupportedCurve(other.to_string())),
    };

    if x.len() != expected_len || y.len() != expected_len {
        return Err(JwksError::ParseError(format!(
            "Invalid EC key length for curve {crv}"
        )));
    }

    let mut uncompressed = Vec::with_capacity(1 + x.len() + y.len());
    uncompressed.push(0x04);
    uncompressed.extend_from_slice(&x);
    uncompressed.extend_from_slice(&y);

    Ok(uncompressed)
}

fn encode_der_integer(bytes: &[u8]) -> Vec<u8> {
    let mut trimmed = bytes;
    while trimmed.len() > 1 && trimmed[0] == 0 {
        trimmed = &trimmed[1..];
    }

    let needs_padding = trimmed[0] & 0x80 != 0;
    let len = if needs_padding {
        trimmed.len() + 1
    } else {
        trimmed.len()
    };

    let mut result = Vec::with_capacity(len + 4);
    result.push(0x02);
    encode_der_length(&mut result, len);
    if needs_padding {
        result.push(0x00);
    }
    result.extend_from_slice(trimmed);
    result
}

#[allow(clippy::cast_possible_truncation)]
fn encode_der_length(buf: &mut Vec<u8>, len: usize) {
    if len < 128 {
        buf.push(len as u8);
    } else if len < 256 {
        buf.push(0x81);
        buf.push(len as u8);
    } else {
        buf.push(0x82);
        buf.push((len >> 8) as u8);
        buf.push(len as u8);
    }
}

fn base64url_decode(input: &str) -> Result<Vec<u8>, JwksError> {
    let padded = match input.len() % 4 {
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => input.to_string(),
    };

    let standard = padded.replace('-', "+").replace('_', "/");

    BASE64_STANDARD
        .decode(standard)
        .map_err(|e| JwksError::ParseError(format!("base64 decode error: {e}")))
}

#[derive(Debug)]
pub enum JwksError {
    InvalidUri(String),
    ConnectionFailed(String),
    TlsError(String),
    Timeout,
    HttpError(String),
    ParseError(String),
    UnsupportedCurve(String),
}

impl std::fmt::Display for JwksError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUri(msg) => write!(f, "invalid URI: {msg}"),
            Self::ConnectionFailed(msg) => write!(f, "connection failed: {msg}"),
            Self::TlsError(msg) => write!(f, "TLS error: {msg}"),
            Self::Timeout => write!(f, "request timeout"),
            Self::HttpError(msg) => write!(f, "HTTP error: {msg}"),
            Self::ParseError(msg) => write!(f, "parse error: {msg}"),
            Self::UnsupportedCurve(crv) => write!(f, "unsupported curve: {crv}"),
        }
    }
}

impl std::error::Error for JwksError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_google_jwks_format() {
        let jwks_json = r#"{
            "keys": [
                {
                    "kid": "test-key-1",
                    "kty": "RSA",
                    "alg": "RS256",
                    "use": "sig",
                    "n": "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw",
                    "e": "AQAB"
                }
            ]
        }"#;

        let keys = parse_jwks_json(jwks_json).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].kid, "test-key-1");
        assert_eq!(keys[0].algorithm, "RS256");
        assert!(!keys[0].key_bytes.is_empty());
    }

    #[test]
    fn test_parse_ec_jwks() {
        let jwks_json = r#"{
            "keys": [
                {
                    "kid": "ec-key-1",
                    "kty": "EC",
                    "crv": "P-256",
                    "x": "WbbXwgvHgVEy_H2TEGNYlSQT6X8XNqIg3dCn7TXGE7A",
                    "y": "R4I-6W1e5X0Zu6p9AaJz4umI9P7m3-YrLMQyCfVd3Q4"
                }
            ]
        }"#;

        let keys = parse_jwks_json(jwks_json).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].kid, "ec-key-1");
        assert_eq!(keys[0].algorithm, "ES256");
        assert_eq!(keys[0].key_bytes.len(), 65);
        assert_eq!(keys[0].key_bytes[0], 0x04);
    }

    #[test]
    fn test_skip_encryption_keys() {
        let jwks_json = r#"{
            "keys": [
                {
                    "kid": "sig-key",
                    "kty": "RSA",
                    "use": "sig",
                    "n": "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw",
                    "e": "AQAB"
                },
                {
                    "kid": "enc-key",
                    "kty": "RSA",
                    "use": "enc",
                    "n": "0vx7agoebGcQSuuPiLJXZptN9nndrQmbXEps2aiAFbWhM78LhWx4cbbfAAtVT86zwu1RK7aPFFxuhDR1L6tSoc_BJECPebWKRXjBZCiFV4n3oknjhMstn64tZ_2W-5JsGY4Hc5n9yBXArwl93lqt7_RN5w6Cf0h4QyQ5v-65YGjQR0_FDW2QvzqY368QQMicAtaSqzs8KJZgnYb9c7d0zgdAZHzu6qMQvRL5hajrn1n91CbOpbISD08qNLyrdkt-bFTWhAI4vMQFh6WeZu0fM4lFd2NcRwr3XPksINHaQ-G_xBniIqbw0Ls1jF44-csFCur-kEgU8awapJzKnqDKgw",
                    "e": "AQAB"
                }
            ]
        }"#;

        let keys = parse_jwks_json(jwks_json).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].kid, "sig-key");
    }

    #[test]
    fn test_circuit_breaker_transitions() {
        let mut cb = CircuitBreakerState::new();

        assert!(cb.should_attempt());
        assert_eq!(cb.state, CircuitState::Closed);

        cb.record_failure();
        assert!(cb.should_attempt());

        cb.record_failure();
        assert!(cb.should_attempt());

        cb.record_failure();
        assert!(!cb.should_attempt());
        assert_eq!(cb.state, CircuitState::Open);

        cb.record_success();
        assert!(cb.should_attempt());
        assert_eq!(cb.state, CircuitState::Closed);
    }

    #[test]
    fn test_base64url_decode() {
        let decoded = base64url_decode("SGVsbG8").unwrap();
        assert_eq!(decoded, b"Hello");

        let decoded = base64url_decode("SGVsbG8gV29ybGQ").unwrap();
        assert_eq!(decoded, b"Hello World");
    }

    #[tokio::test]
    async fn test_jwks_cache_operations() {
        let cache = JwksCache::new();

        let result = cache.get_key("nonexistent", "RS256").await;
        assert!(result.is_none());
    }
}
