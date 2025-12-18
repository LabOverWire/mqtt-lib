use super::jwks::{JwkKey, JwksCache, JwksEndpointConfig};
use super::jwt::{JwtClaims, JwtHeader, JwtVerifier};
use crate::broker::acl::{AclManager, FederatedRoleEntry};
use crate::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult};
use crate::broker::config::{
    ClaimPattern, FederatedAuthMode, JwtIssuerConfig, JwtKeySource, JwtRoleMapping,
};
use crate::error::Result;
use crate::packet::connect::ConnectPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use base64::prelude::*;
use ring::{hmac, signature};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, warn};

struct IssuerState {
    config: JwtIssuerConfig,
    static_verifier: Option<JwtVerifier>,
}

fn extract_domain(issuer: &str) -> &str {
    issuer
        .strip_prefix("https://")
        .or_else(|| issuer.strip_prefix("http://"))
        .unwrap_or(issuer)
        .split('/')
        .next()
        .unwrap_or(issuer)
}

fn compute_user_id(
    issuer: &str,
    sub: Option<&str>,
    prefix: Option<&str>,
) -> std::result::Result<String, JwtError> {
    let sub = sub.ok_or(JwtError::InvalidClaim("missing sub claim"))?;
    if sub.is_empty() {
        return Err(JwtError::InvalidClaim("empty sub claim"));
    }
    if sub.len() > 256 {
        return Err(JwtError::InvalidClaim("sub claim too long"));
    }
    if sub.contains(':') {
        return Err(JwtError::InvalidClaim(
            "sub claim contains invalid character ':'",
        ));
    }
    if sub.chars().any(char::is_control) {
        return Err(JwtError::InvalidClaim(
            "sub claim contains control characters",
        ));
    }
    let prefix = prefix.unwrap_or_else(|| extract_domain(issuer));
    Ok(format!("{}:{}", prefix, sub))
}

pub struct FederatedJwtAuthProvider {
    issuers: HashMap<String, IssuerState>,
    jwks_cache: Arc<JwksCache>,
    acl_manager: Option<Arc<RwLock<AclManager>>>,
    clock_skew_secs: u64,
}

impl FederatedJwtAuthProvider {
    #[allow(clippy::missing_errors_doc)]
    pub fn new(issuers: Vec<JwtIssuerConfig>) -> Result<Self> {
        let mut issuer_map = HashMap::new();
        let mut jwks_cache = JwksCache::new();

        for config in issuers {
            if !config.enabled {
                continue;
            }

            let static_verifier = match &config.key_source {
                JwtKeySource::StaticFile { algorithm, path } => {
                    Some(load_static_verifier(*algorithm, path)?)
                }
                JwtKeySource::Jwks {
                    uri,
                    fallback_key_file,
                    refresh_interval_secs,
                    cache_ttl_secs,
                } => {
                    jwks_cache.add_endpoint(JwksEndpointConfig {
                        uri: uri.clone(),
                        issuer: config.issuer.clone(),
                        refresh_interval: Duration::from_secs(*refresh_interval_secs),
                        cache_ttl: Duration::from_secs(*cache_ttl_secs),
                        request_timeout: Duration::from_secs(10),
                    });

                    load_fallback_verifier(fallback_key_file).ok()
                }
            };

            issuer_map.insert(
                config.issuer.clone(),
                IssuerState {
                    config,
                    static_verifier,
                },
            );
        }

        Ok(Self {
            issuers: issuer_map,
            jwks_cache: Arc::new(jwks_cache),
            acl_manager: None,
            clock_skew_secs: 60,
        })
    }

    #[must_use]
    pub fn with_acl_manager(mut self, acl_manager: Arc<RwLock<AclManager>>) -> Self {
        self.acl_manager = Some(acl_manager);
        self
    }

    #[must_use]
    pub fn with_clock_skew(mut self, seconds: u64) -> Self {
        self.clock_skew_secs = seconds;
        self
    }

    #[allow(clippy::missing_errors_doc)]
    pub async fn initial_fetch(&self) -> Result<()> {
        self.jwks_cache.initial_fetch().await
    }

    pub fn start_background_refresh(&self) -> tokio::task::JoinHandle<()> {
        self.jwks_cache.start_background_refresh()
    }

    pub fn shutdown(&self) {
        self.jwks_cache.shutdown();
    }

    async fn verify_jwt(&self, token: &[u8]) -> std::result::Result<(JwtClaims, String), JwtError> {
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

        let claims: JwtClaims = serde_json::from_slice(&payload_json)
            .map_err(|_| JwtError::InvalidFormat("invalid payload JSON"))?;

        let issuer = claims.iss.clone().unwrap_or_default();
        let issuer_state = self
            .issuers
            .get(&issuer)
            .ok_or_else(|| JwtError::UnknownIssuer(issuer.clone()))?;

        let message = format!("{}.{}", parts[0], parts[1]);
        let message_bytes = message.as_bytes();

        let verified = if let Some(ref kid) = header.kid {
            if let Some(jwk_key) = self
                .jwks_cache
                .get_key_by_issuer(&issuer, kid, &header.alg)
                .await
            {
                self.verify_with_jwk_key(&jwk_key, &header.alg, message_bytes, &signature_bytes)
            } else if let Some(ref verifier) = issuer_state.static_verifier {
                self.verify_with_static(verifier, &header.alg, message_bytes, &signature_bytes)
            } else {
                return Err(JwtError::KeyNotFound(kid.clone()));
            }
        } else if let Some(ref verifier) = issuer_state.static_verifier {
            self.verify_with_static(verifier, &header.alg, message_bytes, &signature_bytes)
        } else {
            let keys = self.jwks_cache.get_keys_for_issuer(&issuer).await;
            let mut found = false;
            for key in keys {
                if key.algorithm == header.alg
                    && self.verify_with_jwk_key(&key, &header.alg, message_bytes, &signature_bytes)
                {
                    found = true;
                    break;
                }
            }
            found
        };

        if !verified {
            return Err(JwtError::InvalidSignature);
        }

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let clock_skew = issuer_state
            .config
            .clock_skew_secs
            .max(self.clock_skew_secs);

        if let Some(exp) = claims.exp {
            if now > exp + clock_skew {
                return Err(JwtError::Expired);
            }
        }

        if let Some(nbf) = claims.nbf {
            if now + clock_skew < nbf {
                return Err(JwtError::NotYetValid);
            }
        }

        if let Some(ref required_aud) = issuer_state.config.audience {
            match &claims.aud {
                Some(aud) if aud == required_aud => {}
                _ => return Err(JwtError::InvalidClaim("audience mismatch")),
            }
        }

        Ok((claims, issuer))
    }

    fn verify_with_jwk_key(&self, key: &JwkKey, alg: &str, message: &[u8], sig: &[u8]) -> bool {
        match alg {
            "RS256" => {
                let public_key = signature::UnparsedPublicKey::new(
                    &signature::RSA_PKCS1_2048_8192_SHA256,
                    &key.key_bytes,
                );
                public_key.verify(message, sig).is_ok()
            }
            "ES256" => {
                let public_key = signature::UnparsedPublicKey::new(
                    &signature::ECDSA_P256_SHA256_FIXED,
                    &key.key_bytes,
                );
                public_key.verify(message, sig).is_ok()
            }
            _ => false,
        }
    }

    fn verify_with_static(
        &self,
        verifier: &JwtVerifier,
        alg: &str,
        message: &[u8],
        sig: &[u8],
    ) -> bool {
        if verifier.algorithm() != alg {
            return false;
        }
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

    fn extract_roles_for_mode(&self, claims: &JwtClaims, issuer: &str) -> HashSet<String> {
        let Some(issuer_state) = self.issuers.get(issuer) else {
            return HashSet::new();
        };

        let config = &issuer_state.config;

        match config.auth_mode {
            FederatedAuthMode::IdentityOnly => HashSet::new(),

            FederatedAuthMode::ClaimBinding => {
                let mut roles: HashSet<String> = config.default_roles.iter().cloned().collect();
                for mapping in &config.role_mappings {
                    if self.mapping_matches(claims, mapping) {
                        roles.extend(mapping.assign_roles.iter().cloned());
                    }
                }
                roles
            }

            FederatedAuthMode::TrustedRoles => {
                let mut roles = HashSet::new();
                let claim_paths: Vec<&str> = if config.trusted_role_claims.is_empty() {
                    vec!["roles", "groups", "realm_access.roles"]
                } else {
                    config
                        .trusted_role_claims
                        .iter()
                        .map(String::as_str)
                        .collect()
                };

                for path in claim_paths {
                    if let Some(values) = claims.get_claim_as_array(path) {
                        roles.extend(values);
                    } else if let Some(value) = claims.get_claim_as_string(path) {
                        roles.insert(value);
                    }
                }
                roles.extend(config.default_roles.iter().cloned());
                roles
            }
        }
    }

    fn mapping_matches(&self, claims: &JwtClaims, mapping: &JwtRoleMapping) -> bool {
        if let Some(value) = claims.get_claim_as_string(&mapping.claim_path) {
            return mapping.pattern.matches(&value);
        }

        if let Some(values) = claims.get_claim_as_array(&mapping.claim_path) {
            for value in values {
                if mapping.pattern.matches(&value) {
                    return true;
                }
            }
        }

        matches!(mapping.pattern, ClaimPattern::Any)
    }

    fn get_issuer_config(&self, issuer: &str) -> Option<&JwtIssuerConfig> {
        self.issuers.get(issuer).map(|s| &s.config)
    }
}

impl AuthProvider for FederatedJwtAuthProvider {
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
        user_id: Option<&'a str>,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            let Some(ref acl) = self.acl_manager else {
                return Ok(true);
            };
            let acl_guard = acl.read().await;
            Ok(acl_guard.check_publish(user_id, topic).await)
        })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        _client_id: &str,
        user_id: Option<&'a str>,
        topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + 'a>> {
        Box::pin(async move {
            let Some(ref acl) = self.acl_manager else {
                return Ok(true);
            };
            let acl_guard = acl.read().await;
            Ok(acl_guard.check_subscribe(user_id, topic_filter).await)
        })
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
                debug!(method = %method, "Federated JWT auth received non-JWT method");
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

            match self.verify_jwt(token).await {
                Ok((claims, issuer)) => {
                    let config = self.get_issuer_config(&issuer);
                    let issuer_prefix = config.and_then(|c| c.issuer_prefix.as_deref());
                    let session_scoped = config.map_or(true, |c| c.session_scoped_roles);
                    let auth_mode = config.map_or(FederatedAuthMode::default(), |c| c.auth_mode);

                    let user_id =
                        match compute_user_id(&issuer, claims.sub.as_deref(), issuer_prefix) {
                            Ok(id) => id,
                            Err(e) => {
                                warn!(error = %e, "JWT authentication failed: invalid user ID");
                                return Ok(EnhancedAuthResult::fail_with_reason(
                                    method,
                                    ReasonCode::NotAuthorized,
                                    e.to_string(),
                                ));
                            }
                        };

                    let roles = self.extract_roles_for_mode(&claims, &issuer);

                    if let Some(ref acl) = self.acl_manager {
                        let acl_guard = acl.read().await;
                        let entry = FederatedRoleEntry {
                            roles: roles.clone(),
                            issuer: issuer.clone(),
                            mode: auth_mode,
                            session_bound: session_scoped,
                        };
                        acl_guard.set_federated_roles(&user_id, entry).await;
                    }

                    debug!(
                        user_id = %user_id,
                        issuer = %issuer,
                        mode = ?auth_mode,
                        roles = ?roles,
                        "JWT authentication successful"
                    );
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

    fn cleanup_session<'a>(
        &'a self,
        user_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some(ref acl) = self.acl_manager {
                let acl_guard = acl.read().await;
                acl_guard.clear_session_bound_roles(user_id).await;
                debug!(user_id = %user_id, "Cleaned up session-bound federated roles");
            }
        })
    }
}

fn load_static_verifier(
    algorithm: crate::broker::config::JwtAlgorithm,
    path: &PathBuf,
) -> Result<JwtVerifier> {
    let key_data = std::fs::read(path).map_err(|e| {
        crate::error::MqttError::Configuration(format!("Failed to read key file: {e}"))
    })?;

    Ok(match algorithm {
        crate::broker::config::JwtAlgorithm::HS256 => JwtVerifier::Hs256(key_data),
        crate::broker::config::JwtAlgorithm::RS256 => JwtVerifier::Rs256(key_data),
        crate::broker::config::JwtAlgorithm::ES256 => JwtVerifier::Es256(key_data),
    })
}

fn load_fallback_verifier(path: &PathBuf) -> Result<JwtVerifier> {
    let key_data = std::fs::read(path).map_err(|e| {
        crate::error::MqttError::Configuration(format!("Failed to read fallback key file: {e}"))
    })?;
    Ok(JwtVerifier::Rs256(key_data))
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
    UnknownIssuer(String),
    KeyNotFound(String),
    Expired,
    NotYetValid,
    InvalidClaim(&'static str),
}

impl std::fmt::Display for JwtError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidFormat(msg) => write!(f, "invalid JWT format: {msg}"),
            Self::InvalidSignature => write!(f, "invalid signature"),
            Self::UnknownIssuer(iss) => write!(f, "unknown issuer: {iss}"),
            Self::KeyNotFound(kid) => write!(f, "key not found: {kid}"),
            Self::Expired => write!(f, "token expired"),
            Self::NotYetValid => write!(f, "token not yet valid"),
            Self::InvalidClaim(msg) => write!(f, "invalid claim: {msg}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::config::JwtRoleMapping;

    #[test]
    fn test_claim_pattern_matching() {
        assert!(ClaimPattern::Equals("test".into()).matches("test"));
        assert!(!ClaimPattern::Equals("test".into()).matches("other"));

        assert!(ClaimPattern::Contains("@".into()).matches("user@example.com"));
        assert!(!ClaimPattern::Contains("@".into()).matches("noatsign"));

        assert!(ClaimPattern::EndsWith("@example.com".into()).matches("user@example.com"));
        assert!(!ClaimPattern::EndsWith("@example.com".into()).matches("user@other.com"));

        assert!(ClaimPattern::StartsWith("admin".into()).matches("admin@example.com"));
        assert!(!ClaimPattern::StartsWith("admin".into()).matches("user@example.com"));

        assert!(ClaimPattern::Any.matches("anything"));
    }

    #[test]
    fn test_role_mapping_creation() {
        let mapping = JwtRoleMapping::new(
            "email",
            ClaimPattern::EndsWith("@company.com".into()),
            vec!["employee".into()],
        );

        assert_eq!(mapping.claim_path, "email");
        assert_eq!(mapping.assign_roles, vec!["employee"]);
    }
}
