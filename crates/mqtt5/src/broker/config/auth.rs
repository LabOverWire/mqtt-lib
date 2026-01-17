use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    pub allow_anonymous: bool,
    pub password_file: Option<PathBuf>,
    pub acl_file: Option<PathBuf>,
    pub auth_method: AuthMethod,
    pub auth_data: Option<Vec<u8>>,
    pub scram_file: Option<PathBuf>,
    pub jwt_config: Option<JwtConfig>,
    pub federated_jwt_config: Option<FederatedJwtConfig>,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitConfig {
    pub enabled: bool,
    pub max_attempts: u32,
    pub window_secs: u64,
    pub lockout_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 5,
            window_secs: 60,
            lockout_secs: 300,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            allow_anonymous: true,
            password_file: None,
            acl_file: None,
            auth_method: AuthMethod::None,
            auth_data: None,
            scram_file: None,
            jwt_config: None,
            federated_jwt_config: None,
            rate_limit: RateLimitConfig::default(),
        }
    }
}

impl AuthConfig {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_password_file(mut self, path: PathBuf) -> Self {
        self.password_file = Some(path);
        self.auth_method = AuthMethod::Password;
        self
    }

    #[must_use]
    pub fn with_scram_file(mut self, path: PathBuf) -> Self {
        self.scram_file = Some(path);
        self.auth_method = AuthMethod::ScramSha256;
        self
    }

    #[must_use]
    pub fn with_jwt(mut self, config: JwtConfig) -> Self {
        self.jwt_config = Some(config);
        self.auth_method = AuthMethod::Jwt;
        self
    }

    #[must_use]
    pub fn with_federated_jwt(mut self, config: FederatedJwtConfig) -> Self {
        self.federated_jwt_config = Some(config);
        self.auth_method = AuthMethod::JwtFederated;
        self
    }

    #[must_use]
    pub fn with_acl_file(mut self, path: PathBuf) -> Self {
        self.acl_file = Some(path);
        self
    }

    #[must_use]
    pub fn with_allow_anonymous(mut self, allow: bool) -> Self {
        self.allow_anonymous = allow;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    None,
    Password,
    ScramSha256,
    Jwt,
    JwtFederated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JwtAlgorithm {
    HS256,
    RS256,
    ES256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtConfig {
    pub algorithm: JwtAlgorithm,
    pub secret_or_key_file: PathBuf,
    pub issuer: Option<String>,
    pub audience: Option<String>,
    pub clock_skew_secs: u64,
}

impl Default for JwtConfig {
    fn default() -> Self {
        Self {
            algorithm: JwtAlgorithm::HS256,
            secret_or_key_file: PathBuf::new(),
            issuer: None,
            audience: None,
            clock_skew_secs: 60,
        }
    }
}

impl JwtConfig {
    #[must_use]
    pub fn new(algorithm: JwtAlgorithm, secret_or_key_file: PathBuf) -> Self {
        Self {
            algorithm,
            secret_or_key_file,
            issuer: None,
            audience: None,
            clock_skew_secs: 60,
        }
    }

    #[must_use]
    pub fn with_issuer(mut self, issuer: impl Into<String>) -> Self {
        self.issuer = Some(issuer.into());
        self
    }

    #[must_use]
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    #[must_use]
    pub fn with_clock_skew(mut self, seconds: u64) -> Self {
        self.clock_skew_secs = seconds;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum JwtKeySource {
    StaticFile {
        algorithm: JwtAlgorithm,
        path: PathBuf,
    },
    Jwks {
        uri: String,
        fallback_key_file: PathBuf,
        #[serde(default = "default_refresh_interval")]
        refresh_interval_secs: u64,
        #[serde(default = "default_cache_ttl")]
        cache_ttl_secs: u64,
    },
}

fn default_refresh_interval() -> u64 {
    3600
}

fn default_cache_ttl() -> u64 {
    86400
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtIssuerConfig {
    pub name: String,
    pub issuer: String,
    pub key_source: JwtKeySource,
    pub audience: Option<String>,
    #[serde(default = "default_clock_skew")]
    pub clock_skew_secs: u64,
    #[serde(default)]
    pub auth_mode: FederatedAuthMode,
    #[serde(default)]
    pub role_mappings: Vec<JwtRoleMapping>,
    #[serde(default)]
    pub default_roles: Vec<String>,
    #[serde(default)]
    pub trusted_role_claims: Vec<String>,
    #[serde(default = "default_session_scoped_roles")]
    pub session_scoped_roles: bool,
    #[serde(default)]
    pub issuer_prefix: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    #[deprecated(note = "Use auth_mode instead")]
    pub role_merge_mode: RoleMergeMode,
}

fn default_clock_skew() -> u64 {
    60
}

fn default_session_scoped_roles() -> bool {
    true
}

fn default_enabled() -> bool {
    true
}

impl JwtIssuerConfig {
    #[must_use]
    #[allow(deprecated)]
    pub fn new(
        name: impl Into<String>,
        issuer: impl Into<String>,
        key_source: JwtKeySource,
    ) -> Self {
        Self {
            name: name.into(),
            issuer: issuer.into(),
            key_source,
            audience: None,
            clock_skew_secs: 60,
            auth_mode: FederatedAuthMode::default(),
            role_mappings: Vec::new(),
            default_roles: Vec::new(),
            trusted_role_claims: Vec::new(),
            session_scoped_roles: true,
            issuer_prefix: None,
            enabled: true,
            role_merge_mode: RoleMergeMode::default(),
        }
    }

    #[must_use]
    pub fn with_audience(mut self, audience: impl Into<String>) -> Self {
        self.audience = Some(audience.into());
        self
    }

    #[must_use]
    pub fn with_auth_mode(mut self, mode: FederatedAuthMode) -> Self {
        self.auth_mode = mode;
        self
    }

    #[must_use]
    pub fn with_role_mapping(mut self, mapping: JwtRoleMapping) -> Self {
        self.role_mappings.push(mapping);
        self
    }

    #[must_use]
    pub fn with_default_roles(mut self, roles: Vec<String>) -> Self {
        self.default_roles = roles;
        self
    }

    #[must_use]
    pub fn with_trusted_role_claims(mut self, claims: Vec<String>) -> Self {
        self.trusted_role_claims = claims;
        self
    }

    #[must_use]
    pub fn with_session_scoped_roles(mut self, session_scoped: bool) -> Self {
        self.session_scoped_roles = session_scoped;
        self
    }

    #[must_use]
    pub fn with_issuer_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.issuer_prefix = Some(prefix.into());
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum RoleMergeMode {
    #[default]
    Merge,
    Replace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FederatedAuthMode {
    #[default]
    IdentityOnly,
    ClaimBinding,
    TrustedRoles,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtRoleMapping {
    pub claim_path: String,
    pub pattern: ClaimPattern,
    pub assign_roles: Vec<String>,
}

impl JwtRoleMapping {
    #[must_use]
    pub fn new(claim_path: impl Into<String>, pattern: ClaimPattern, roles: Vec<String>) -> Self {
        Self {
            claim_path: claim_path.into(),
            pattern,
            assign_roles: roles,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClaimPattern {
    Equals(String),
    Contains(String),
    EndsWith(String),
    StartsWith(String),
    Regex(String),
    Any,
}

impl ClaimPattern {
    #[must_use]
    pub fn matches(&self, value: &str) -> bool {
        match self {
            Self::Equals(s) => value == s,
            Self::Contains(s) => value.contains(s),
            Self::EndsWith(s) => value.ends_with(s),
            Self::StartsWith(s) => value.starts_with(s),
            Self::Regex(pattern) => regex::Regex::new(pattern)
                .map(|re| re.is_match(value))
                .unwrap_or(false),
            Self::Any => true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FederatedJwtConfig {
    pub issuers: Vec<JwtIssuerConfig>,
    #[serde(default = "default_clock_skew")]
    pub clock_skew_secs: u64,
}
