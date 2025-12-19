use crate::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use crate::error::Result;
use crate::packet::connect::ConnectPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use base64::prelude::*;
use ring::{hmac, signature};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use tracing::{debug, warn};

#[derive(Clone)]
pub enum JwtVerifier {
    Hs256(Vec<u8>),
    Rs256(Vec<u8>),
    Es256(Vec<u8>),
}

impl JwtVerifier {
    #[must_use]
    pub fn algorithm(&self) -> &'static str {
        match self {
            Self::Hs256(_) => "HS256",
            Self::Rs256(_) => "RS256",
            Self::Es256(_) => "ES256",
        }
    }
}

const DEFAULT_CLOCK_SKEW_SECS: u64 = 60;

pub struct JwtAuthProvider {
    verifiers: Vec<JwtVerifier>,
    required_issuer: Option<String>,
    required_audience: Option<String>,
    clock_skew_secs: u64,
}

impl JwtAuthProvider {
    #[must_use]
    pub fn with_hs256_secret(secret: impl AsRef<[u8]>) -> Self {
        Self {
            verifiers: vec![JwtVerifier::Hs256(secret.as_ref().to_vec())],
            required_issuer: None,
            required_audience: None,
            clock_skew_secs: DEFAULT_CLOCK_SKEW_SECS,
        }
    }

    #[must_use]
    pub fn with_rs256_public_key(der: impl AsRef<[u8]>) -> Self {
        Self {
            verifiers: vec![JwtVerifier::Rs256(der.as_ref().to_vec())],
            required_issuer: None,
            required_audience: None,
            clock_skew_secs: DEFAULT_CLOCK_SKEW_SECS,
        }
    }

    #[must_use]
    pub fn with_es256_public_key(der: impl AsRef<[u8]>) -> Self {
        Self {
            verifiers: vec![JwtVerifier::Es256(der.as_ref().to_vec())],
            required_issuer: None,
            required_audience: None,
            clock_skew_secs: DEFAULT_CLOCK_SKEW_SECS,
        }
    }

    #[must_use]
    pub fn add_verifier(mut self, verifier: JwtVerifier) -> Self {
        self.verifiers.push(verifier);
        self
    }

    #[must_use]
    pub fn with_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.required_issuer = Some(issuer.into());
        self
    }

    #[must_use]
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.required_audience = Some(audience.into());
        self
    }

    #[must_use]
    pub fn with_clock_skew(mut self, seconds: u64) -> Self {
        self.clock_skew_secs = seconds;
        self
    }

    fn verify_jwt(&self, token: &[u8]) -> std::result::Result<JwtClaims, JwtError> {
        let token_str =
            std::str::from_utf8(token).map_err(|_| JwtError::InvalidFormat("not UTF-8"))?;

        let parts: Vec<&str> = token_str.split('.').collect();
        if parts.len() != 3 {
            return Err(JwtError::InvalidFormat("expected 3 parts"));
        }

        let header_json = base64url_decode(parts[0])?;
        let payload_json = base64url_decode(parts[1])?;
        let signature_bytes = base64url_decode(parts[2])?;

        let header: JwtHeader = serde_json::from_slice(&header_json)
            .map_err(|_| JwtError::InvalidFormat("invalid header JSON"))?;

        let message = format!("{}.{}", parts[0], parts[1]);
        let message_bytes = message.as_bytes();

        let matching_verifier = self.verifiers.iter().find(|v| v.algorithm() == header.alg);

        let Some(verifier) = matching_verifier else {
            return Err(JwtError::UnsupportedAlgorithm(header.alg));
        };

        if !Self::verify_signature(verifier, message_bytes, &signature_bytes) {
            return Err(JwtError::InvalidSignature);
        }

        let claims: JwtClaims = serde_json::from_slice(&payload_json)
            .map_err(|_| JwtError::InvalidFormat("invalid payload JSON"))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        if let Some(exp) = claims.exp {
            if now > exp + self.clock_skew_secs {
                return Err(JwtError::Expired);
            }
        }

        if let Some(nbf) = claims.nbf {
            if now + self.clock_skew_secs < nbf {
                return Err(JwtError::NotYetValid);
            }
        }

        if let Some(ref required_iss) = self.required_issuer {
            match &claims.iss {
                Some(iss) if iss == required_iss => {}
                _ => return Err(JwtError::InvalidClaim("issuer mismatch")),
            }
        }

        if let Some(ref required_aud) = self.required_audience {
            match &claims.aud {
                Some(aud) if aud == required_aud => {}
                _ => return Err(JwtError::InvalidClaim("audience mismatch")),
            }
        }

        Ok(claims)
    }

    fn verify_signature(verifier: &JwtVerifier, message: &[u8], sig: &[u8]) -> bool {
        match verifier {
            JwtVerifier::Hs256(secret) => {
                let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
                hmac::verify(&key, message, sig).is_ok()
            }
            JwtVerifier::Rs256(public_key) => {
                let key = signature::UnparsedPublicKey::new(
                    &signature::RSA_PKCS1_2048_8192_SHA256,
                    public_key,
                );
                key.verify(message, sig).is_ok()
            }
            JwtVerifier::Es256(public_key) => {
                let key = signature::UnparsedPublicKey::new(
                    &signature::ECDSA_P256_SHA256_FIXED,
                    public_key,
                );
                key.verify(message, sig).is_ok()
            }
        }
    }
}

impl AuthProvider for JwtAuthProvider {
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

        Box::pin(async move {
            if method != "JWT" {
                debug!(method = %method, "JWT auth provider received non-JWT method");
                return Ok(EnhancedAuthResult::fail(
                    method,
                    ReasonCode::BadAuthenticationMethod,
                ));
            }

            let Some(token) = auth_data else {
                warn!("JWT authentication failed: no token provided");
                return Ok(EnhancedAuthResult::fail_with_reason(
                    method,
                    ReasonCode::NotAuthorized,
                    "No JWT token provided".to_string(),
                ));
            };

            match self.verify_jwt(token) {
                Ok(claims) => {
                    let user_id = claims.sub.unwrap_or_default();
                    debug!(user_id = %user_id, "JWT authentication successful");
                    Ok(EnhancedAuthResult::success_with_user(method, user_id))
                }
                Err(e) => {
                    warn!(error = %e, "JWT authentication failed");
                    Ok(EnhancedAuthResult::fail_with_reason(
                        method,
                        ReasonCode::NotAuthorized,
                        e.to_string(),
                    ))
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
        self.authenticate_enhanced(auth_method, auth_data, client_id)
    }
}

fn base64url_decode(input: &str) -> std::result::Result<Vec<u8>, JwtError> {
    let padded = match input.len() % 4 {
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => input.to_string(),
    };

    let standard = padded.replace('-', "+").replace('_', "/");

    BASE64_STANDARD
        .decode(standard)
        .map_err(|_| JwtError::InvalidFormat("invalid base64"))
}

#[derive(Debug)]
enum JwtError {
    InvalidFormat(&'static str),
    InvalidSignature,
    UnsupportedAlgorithm(String),
    Expired,
    NotYetValid,
    InvalidClaim(&'static str),
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFormat(msg) => write!(f, "invalid JWT format: {msg}"),
            Self::InvalidSignature => write!(f, "invalid signature"),
            Self::UnsupportedAlgorithm(alg) => write!(f, "unsupported algorithm: {alg}"),
            Self::Expired => write!(f, "token expired"),
            Self::NotYetValid => write!(f, "token not yet valid"),
            Self::InvalidClaim(msg) => write!(f, "invalid claim: {msg}"),
        }
    }
}

#[derive(serde::Deserialize)]
pub struct JwtHeader {
    pub alg: String,
    pub kid: Option<String>,
}

#[derive(serde::Deserialize, Default, Clone)]
pub struct JwtClaims {
    pub sub: Option<String>,
    pub iss: Option<String>,
    pub aud: Option<String>,
    pub exp: Option<u64>,
    pub nbf: Option<u64>,
    pub email: Option<String>,
    pub groups: Option<Vec<String>>,
    pub roles: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl JwtClaims {
    #[must_use]
    pub fn get_claim(&self, path: &str) -> Option<&serde_json::Value> {
        let parts: Vec<&str> = path.split('.').collect();
        let mut current: Option<&serde_json::Value> = self.extra.get(parts[0]);

        for part in &parts[1..] {
            current = current.and_then(|v| v.get(part));
        }

        current
    }

    #[must_use]
    pub fn get_claim_as_string(&self, path: &str) -> Option<String> {
        if path == "sub" {
            return self.sub.clone();
        }
        if path == "iss" {
            return self.iss.clone();
        }
        if path == "email" {
            return self.email.clone();
        }

        self.get_claim(path).and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            _ => None,
        })
    }

    #[must_use]
    pub fn get_claim_as_array(&self, path: &str) -> Option<Vec<String>> {
        if path == "groups" {
            return self.groups.clone();
        }
        if path == "roles" {
            return self.roles.clone();
        }

        self.get_claim(path).and_then(|v| match v {
            serde_json::Value::Array(arr) => {
                let strings: Vec<String> = arr
                    .iter()
                    .filter_map(|item| item.as_str().map(String::from))
                    .collect();
                if strings.is_empty() {
                    None
                } else {
                    Some(strings)
                }
            }
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_hs256_token(secret: &[u8], claims: &str) -> String {
        let header = r#"{"alg":"HS256","typ":"JWT"}"#;
        let header_b64 = base64url_encode(header.as_bytes());
        let claims_b64 = base64url_encode(claims.as_bytes());
        let message = format!("{header_b64}.{claims_b64}");

        let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
        let signature = hmac::sign(&key, message.as_bytes());
        let sig_b64 = base64url_encode(signature.as_ref());

        format!("{message}.{sig_b64}")
    }

    fn base64url_encode(data: &[u8]) -> String {
        BASE64_STANDARD
            .encode(data)
            .replace('+', "-")
            .replace('/', "_")
            .trim_end_matches('=')
            .to_string()
    }

    #[test]
    fn test_valid_hs256_token() {
        let secret = b"super-secret-key-for-testing-jwt";
        let provider = JwtAuthProvider::with_hs256_secret(secret);

        let claims = r#"{"sub":"user123","iss":"test","exp":9999999999}"#;
        let token = create_hs256_token(secret, claims);

        let result = provider.verify_jwt(token.as_bytes());
        assert!(result.is_ok());
        let claims = result.unwrap();
        assert_eq!(claims.sub, Some("user123".to_string()));
    }

    #[test]
    fn test_expired_token() {
        let secret = b"super-secret-key-for-testing-jwt";
        let provider = JwtAuthProvider::with_hs256_secret(secret);

        let claims = r#"{"sub":"user123","exp":1}"#;
        let token = create_hs256_token(secret, claims);

        let result = provider.verify_jwt(token.as_bytes());
        assert!(matches!(result, Err(JwtError::Expired)));
    }

    #[test]
    fn test_invalid_signature() {
        let secret = b"super-secret-key-for-testing-jwt";
        let wrong_secret = b"wrong-secret-key";
        let provider = JwtAuthProvider::with_hs256_secret(secret);

        let claims = r#"{"sub":"user123","exp":9999999999}"#;
        let token = create_hs256_token(wrong_secret, claims);

        let result = provider.verify_jwt(token.as_bytes());
        assert!(matches!(result, Err(JwtError::InvalidSignature)));
    }

    #[test]
    fn test_issuer_validation() {
        let secret = b"super-secret-key-for-testing-jwt";
        let provider = JwtAuthProvider::with_hs256_secret(secret).with_issuer("expected-issuer");

        let claims = r#"{"sub":"user123","iss":"wrong-issuer","exp":9999999999}"#;
        let token = create_hs256_token(secret, claims);

        let result = provider.verify_jwt(token.as_bytes());
        assert!(matches!(result, Err(JwtError::InvalidClaim(_))));

        let claims = r#"{"sub":"user123","iss":"expected-issuer","exp":9999999999}"#;
        let token = create_hs256_token(secret, claims);

        let result = provider.verify_jwt(token.as_bytes());
        assert!(result.is_ok());
    }

    #[test]
    fn test_audience_validation() {
        let secret = b"super-secret-key-for-testing-jwt";
        let provider = JwtAuthProvider::with_hs256_secret(secret).with_audience("mqtt-broker");

        let claims = r#"{"sub":"user123","aud":"mqtt-broker","exp":9999999999}"#;
        let token = create_hs256_token(secret, claims);

        let result = provider.verify_jwt(token.as_bytes());
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_enhanced_auth_success() {
        let secret = b"super-secret-key-for-testing-jwt";
        let provider = JwtAuthProvider::with_hs256_secret(secret);

        let claims = r#"{"sub":"user123","exp":9999999999}"#;
        let token = create_hs256_token(secret, claims);

        let result = provider
            .authenticate_enhanced("JWT", Some(token.as_bytes()), "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Success
        );
    }

    #[tokio::test]
    async fn test_enhanced_auth_wrong_method() {
        let secret = b"super-secret-key-for-testing-jwt";
        let provider = JwtAuthProvider::with_hs256_secret(secret);

        let result = provider
            .authenticate_enhanced("SCRAM-SHA-256", None, "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Failed
        );
        assert_eq!(result.reason_code, ReasonCode::BadAuthenticationMethod);
    }

    #[tokio::test]
    async fn test_enhanced_auth_no_token() {
        let secret = b"super-secret-key-for-testing-jwt";
        let provider = JwtAuthProvider::with_hs256_secret(secret);

        let result = provider
            .authenticate_enhanced("JWT", None, "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Failed
        );
        assert_eq!(result.reason_code, ReasonCode::NotAuthorized);
    }
}
