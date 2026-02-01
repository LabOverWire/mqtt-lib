//! Authentication and authorization for the MQTT broker

mod composite;
mod providers;
mod rate_limit;

pub use composite::CompositeAuthProvider;
pub use providers::{
    AllowAllAuthProvider, CertificateAuthProvider, ComprehensiveAuthProvider, PasswordAuthProvider,
};
pub use rate_limit::{AuthRateLimiter, RateLimitedAuthProvider};

use crate::error::Result;
use crate::packet::connect::ConnectPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use std::collections::HashSet;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;

use super::config::RoleMergeMode;

/// Authentication result from an auth provider
#[derive(Debug, Clone)]
pub struct AuthResult {
    pub authenticated: bool,
    pub reason_code: ReasonCode,
    pub reason_string: Option<String>,
    pub user_id: Option<String>,
}

impl AuthResult {
    #[must_use]
    pub fn success() -> Self {
        Self {
            authenticated: true,
            reason_code: ReasonCode::Success,
            reason_string: None,
            user_id: None,
        }
    }

    #[must_use]
    pub fn success_with_user(user_id: impl Into<String>) -> Self {
        Self {
            authenticated: true,
            reason_code: ReasonCode::Success,
            reason_string: None,
            user_id: Some(user_id.into()),
        }
    }

    #[must_use]
    pub fn fail(reason_code: ReasonCode) -> Self {
        Self {
            authenticated: false,
            reason_code,
            reason_string: None,
            user_id: None,
        }
    }

    #[must_use]
    pub fn fail_with_reason(reason_code: ReasonCode, reason: impl Into<String>) -> Self {
        Self {
            authenticated: false,
            reason_code,
            reason_string: Some(reason.into()),
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
    pub fn success(auth_method: impl Into<String>) -> Self {
        Self {
            status: EnhancedAuthStatus::Success,
            reason_code: ReasonCode::Success,
            auth_method: auth_method.into(),
            auth_data: None,
            reason_string: None,
            user_id: None,
            roles: None,
            role_merge_mode: None,
        }
    }

    #[must_use]
    pub fn success_with_user(auth_method: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            status: EnhancedAuthStatus::Success,
            reason_code: ReasonCode::Success,
            auth_method: auth_method.into(),
            auth_data: None,
            reason_string: None,
            user_id: Some(user_id.into()),
            roles: None,
            role_merge_mode: None,
        }
    }

    #[must_use]
    pub fn success_with_user_and_roles(
        auth_method: impl Into<String>,
        user_id: impl Into<String>,
        roles: HashSet<String>,
        merge_mode: RoleMergeMode,
    ) -> Self {
        Self {
            status: EnhancedAuthStatus::Success,
            reason_code: ReasonCode::Success,
            auth_method: auth_method.into(),
            auth_data: None,
            reason_string: None,
            user_id: Some(user_id.into()),
            roles: Some(roles),
            role_merge_mode: Some(merge_mode),
        }
    }

    #[must_use]
    pub fn continue_auth(auth_method: impl Into<String>, auth_data: Option<Vec<u8>>) -> Self {
        Self {
            status: EnhancedAuthStatus::Continue,
            reason_code: ReasonCode::ContinueAuthentication,
            auth_method: auth_method.into(),
            auth_data,
            reason_string: None,
            user_id: None,
            roles: None,
            role_merge_mode: None,
        }
    }

    #[must_use]
    pub fn fail(auth_method: impl Into<String>, reason_code: ReasonCode) -> Self {
        Self {
            status: EnhancedAuthStatus::Failed,
            reason_code,
            auth_method: auth_method.into(),
            auth_data: None,
            reason_string: None,
            user_id: None,
            roles: None,
            role_merge_mode: None,
        }
    }

    #[must_use]
    pub fn fail_with_reason(
        auth_method: impl Into<String>,
        reason_code: ReasonCode,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            status: EnhancedAuthStatus::Failed,
            reason_code,
            auth_method: auth_method.into(),
            auth_data: None,
            reason_string: Some(reason.into()),
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

    fn authorize_publish<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

    fn authorize_subscribe<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

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

        let can_publish = provider.authorize_publish("test", None, "test/topic").await;
        assert!(can_publish);

        let can_subscribe = provider.authorize_subscribe("test", None, "test/+").await;
        assert!(can_subscribe);
    }

    #[tokio::test]
    async fn test_password_provider() {
        let provider = PasswordAuthProvider::new();
        provider.add_user("alice".to_string(), "secret123").unwrap();

        let addr = "127.0.0.1:12345".parse().unwrap();

        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("test-client"));
        connect.username = Some("alice".to_string());
        connect.password = Some("secret123".as_bytes().to_vec());

        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        connect.password = Some("wrong".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::BadUsernameOrPassword);

        connect.password = None;
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);

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

        assert_ne!(hash, password);
        assert!(PasswordAuthProvider::verify_password(password, &hash));
        assert!(!PasswordAuthProvider::verify_password("wrong", &hash));
    }

    #[tokio::test]
    async fn test_async_user_management() {
        let provider = PasswordAuthProvider::new();

        assert_eq!(provider.user_count(), 0);
        assert!(!provider.has_user("alice"));

        provider.add_user("alice".to_string(), "secret123").unwrap();
        assert_eq!(provider.user_count(), 1);
        assert!(provider.has_user("alice"));

        let hash = PasswordAuthProvider::hash_password("password456").unwrap();
        provider.add_user_with_hash("bob".to_string(), hash);
        assert_eq!(provider.user_count(), 2);
        assert!(provider.has_user("bob"));

        assert!(provider.remove_user("alice"));
        assert_eq!(provider.user_count(), 1);
        assert!(!provider.has_user("alice"));

        assert!(!provider.remove_user("charlie"));
        assert_eq!(provider.user_count(), 1);
    }

    #[tokio::test]
    async fn test_file_based_authentication() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        let alice_hash = PasswordAuthProvider::hash_password("secret123").unwrap();
        let bob_hash = PasswordAuthProvider::hash_password("password456").unwrap();

        writeln!(temp_file, "# Password file").unwrap();
        writeln!(temp_file, "alice:{alice_hash}").unwrap();
        writeln!(temp_file, "bob:{bob_hash}").unwrap();
        writeln!(temp_file, "# Comment line\n").unwrap();
        writeln!(temp_file, "invalid_line_without_colon").unwrap();
        temp_file.flush().unwrap();

        let provider = PasswordAuthProvider::from_file(temp_file.path())
            .await
            .unwrap();
        assert_eq!(provider.user_count(), 2);
        assert!(provider.has_user("alice"));
        assert!(provider.has_user("bob"));

        let addr = "127.0.0.1:12345".parse().unwrap();

        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("test-client"));
        connect.username = Some("alice".to_string());
        connect.password = Some("secret123".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        connect.username = Some("bob".to_string());
        connect.password = Some("password456".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("bob".to_string()));

        connect.password = Some("wrong".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);

        connect.username = Some("charlie".to_string());
        connect.password = Some("password".as_bytes().to_vec());
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
    }

    #[tokio::test]
    async fn test_password_file_reload() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        let alice_hash = PasswordAuthProvider::hash_password("secret123").unwrap();
        writeln!(temp_file, "alice:{alice_hash}").unwrap();
        temp_file.flush().unwrap();

        let provider = PasswordAuthProvider::from_file(temp_file.path())
            .await
            .unwrap();
        assert_eq!(provider.user_count(), 1);

        let bob_hash = PasswordAuthProvider::hash_password("password456").unwrap();
        writeln!(temp_file, "bob:{bob_hash}").unwrap();
        temp_file.flush().unwrap();

        provider.load_password_file().await.unwrap();
        assert_eq!(provider.user_count(), 2);
        assert!(provider.has_user("alice"));
        assert!(provider.has_user("bob"));
    }

    #[tokio::test]
    async fn test_comprehensive_auth_provider() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut password_file = NamedTempFile::new().unwrap();
        let alice_hash = PasswordAuthProvider::hash_password("secret123").unwrap();
        let bob_hash = PasswordAuthProvider::hash_password("password456").unwrap();
        writeln!(password_file, "alice:{alice_hash}").unwrap();
        writeln!(password_file, "bob:{bob_hash}").unwrap();
        password_file.flush().unwrap();

        let mut acl_file = NamedTempFile::new().unwrap();
        writeln!(acl_file, "user alice topic sensors/+ permission read").unwrap();
        writeln!(acl_file, "user bob topic actuators/# permission write").unwrap();
        writeln!(acl_file, "user * topic public/# permission readwrite").unwrap();
        acl_file.flush().unwrap();

        let auth = ComprehensiveAuthProvider::from_files(password_file.path(), acl_file.path())
            .await
            .unwrap();

        let addr = "127.0.0.1:12345".parse().unwrap();

        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("alice-client"));
        connect.username = Some("alice".to_string());
        connect.password = Some("secret123".as_bytes().to_vec());

        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        assert!(
            !auth
                .authorize_publish("alice-client", Some("alice"), "sensors/temp")
                .await
        );
        assert!(
            auth.authorize_subscribe("alice-client", Some("alice"), "sensors/temp")
                .await
        );
        assert!(
            auth.authorize_publish("alice-client", Some("alice"), "public/announcements")
                .await
        );

        connect.username = Some("bob".to_string());
        connect.password = Some("password456".as_bytes().to_vec());

        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("bob".to_string()));

        assert!(
            auth.authorize_publish("bob-client", Some("bob"), "actuators/fan")
                .await
        );
        assert!(
            !auth
                .authorize_subscribe("bob-client", Some("bob"), "actuators/fan")
                .await
        );
        assert!(
            auth.authorize_publish("bob-client", Some("bob"), "public/messages")
                .await
        );

        connect.username = Some("charlie".to_string());
        connect.password = Some("wrongpass".as_bytes().to_vec());

        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);

        auth.reload().await.unwrap();
    }

    #[tokio::test]
    async fn test_comprehensive_auth_with_allow_all_acl() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut password_file = NamedTempFile::new().unwrap();
        let alice_hash = PasswordAuthProvider::hash_password("secret123").unwrap();
        writeln!(password_file, "alice:{alice_hash}").unwrap();
        password_file.flush().unwrap();

        let auth =
            ComprehensiveAuthProvider::with_password_file_and_allow_all_acl(password_file.path())
                .await
                .unwrap();

        let addr = "127.0.0.1:12345".parse().unwrap();

        let mut connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("alice-client"));
        connect.username = Some("alice".to_string());
        connect.password = Some("secret123".as_bytes().to_vec());

        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);

        assert!(
            auth.authorize_publish("alice-client", Some("alice"), "any/topic")
                .await
        );
        assert!(
            auth.authorize_subscribe("alice-client", Some("alice"), "any/topic")
                .await
        );

        connect.password = Some("wrong".as_bytes().to_vec());
        let result = auth.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
    }

    #[tokio::test]
    async fn test_certificate_provider() {
        let provider = CertificateAuthProvider::new();
        let fingerprint = "1234567890123456789012345678901234567890123456789012345678901234";
        provider.add_certificate(fingerprint, "alice");

        let addr = "127.0.0.1:12345".parse().unwrap();

        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new(format!(
            "cert:{fingerprint}"
        )));
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        let unknown_fingerprint =
            "c3d4e5f6789012345678901234567890abcdef123456789012345678901234567b";
        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new(format!(
            "cert:{unknown_fingerprint}"
        )));
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::NotAuthorized);

        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new(
            "not-cert-format".to_string(),
        ));
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::BadUsernameOrPassword);
    }

    #[test]
    fn test_certificate_fingerprint_calculation() {
        let test_data = b"test certificate data";
        let fingerprint = CertificateAuthProvider::calculate_fingerprint(test_data);

        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.chars().all(|c| c.is_ascii_hexdigit()));

        let fingerprint2 = CertificateAuthProvider::calculate_fingerprint(test_data);
        assert_eq!(fingerprint, fingerprint2);

        let different_data = b"different certificate data";
        let different_fingerprint = CertificateAuthProvider::calculate_fingerprint(different_data);
        assert_ne!(fingerprint, different_fingerprint);
    }

    #[tokio::test]
    async fn test_certificate_management() {
        let provider = CertificateAuthProvider::new();

        assert_eq!(provider.cert_count(), 0);
        assert!(!provider.has_certificate("abc123"));

        let fingerprint = "1234567890123456789012345678901234567890123456789012345678901234";
        provider.add_certificate(fingerprint, "alice");
        assert_eq!(provider.cert_count(), 1);
        assert!(provider.has_certificate(fingerprint));

        assert!(provider.has_certificate(&fingerprint.to_uppercase()));

        assert!(provider.remove_certificate(fingerprint));
        assert_eq!(provider.cert_count(), 0);
        assert!(!provider.has_certificate(fingerprint));

        assert!(!provider.remove_certificate("nonexistent"));
    }

    #[tokio::test]
    async fn test_certificate_file_loading() {
        use std::io::Write;
        use tempfile::NamedTempFile;

        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "# Certificate fingerprint mappings").unwrap();
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
        writeln!(temp_file, "# Another comment\n").unwrap();
        writeln!(temp_file, "invalid:line:too:many:colons").unwrap();
        writeln!(temp_file, "short:charlie").unwrap();
        writeln!(
            temp_file,
            "not_hex_zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz:dave"
        )
        .unwrap();
        writeln!(
            temp_file,
            "c3d4e5f6789012345678901234567890123456789012345678901234567890abcd:"
        )
        .unwrap();
        temp_file.flush().unwrap();

        let provider = CertificateAuthProvider::from_file(temp_file.path())
            .await
            .unwrap();
        assert_eq!(provider.cert_count(), 2);
        assert!(provider
            .has_certificate("1234567890123456789012345678901234567890123456789012345678901234"));
        assert!(provider
            .has_certificate("abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd"));

        let addr = "127.0.0.1:12345".parse().unwrap();

        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new(
            "cert:1234567890123456789012345678901234567890123456789012345678901234",
        ));
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("alice".to_string()));

        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new(
            "cert:abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd",
        ));
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
        assert_eq!(result.user_id, Some("bob".to_string()));

        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new(
            "cert:unknown12345678901234567890123456789012345678901234567890123456",
        ));
        let result = provider.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
    }

    #[tokio::test]
    async fn test_certificate_file_reload() {
        use std::io::Write;
        use tempfile::NamedTempFile;

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
        assert_eq!(provider.cert_count(), 1);

        writeln!(
            temp_file,
            "abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd:bob"
        )
        .unwrap();
        temp_file.flush().unwrap();

        provider.load_cert_file().await.unwrap();
        assert_eq!(provider.cert_count(), 2);
        assert!(provider
            .has_certificate("1234567890123456789012345678901234567890123456789012345678901234"));
        assert!(provider
            .has_certificate("abcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcdefabcd"));
    }

    #[test]
    fn test_certificate_common_name_extraction() {
        let empty_cert = b"";
        let result = CertificateAuthProvider::extract_common_name(empty_cert);
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());

        let fake_cert_with_cn = b"Some certificate data CN=test.example.com,O=Test Org more data";
        let result = CertificateAuthProvider::extract_common_name(fake_cert_with_cn);
        assert!(result.is_ok());
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
