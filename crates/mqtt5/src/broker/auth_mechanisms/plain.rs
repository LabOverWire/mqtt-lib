use crate::broker::auth::{AuthProvider, AuthResult, EnhancedAuthResult, PasswordAuthProvider};
use crate::error::Result;
use crate::packet::connect::ConnectPacket;
use crate::protocol::v5::reason_codes::ReasonCode;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tracing::{debug, warn};

pub trait PasswordCredentialStore: Send + Sync {
    fn verify_password(&self, username: &str, password: &str) -> bool;
}

impl PasswordCredentialStore for PasswordAuthProvider {
    fn verify_password(&self, username: &str, password: &str) -> bool {
        self.verify_user_password_blocking(username, password)
    }
}

pub struct PlainAuthProvider<S: PasswordCredentialStore> {
    store: Arc<S>,
}

impl<S: PasswordCredentialStore> PlainAuthProvider<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self { store }
    }
}

impl PlainAuthProvider<PasswordAuthProvider> {
    pub fn from_password_provider(provider: Arc<PasswordAuthProvider>) -> Self {
        Self { store: provider }
    }
}

fn parse_plain_credentials(data: &[u8]) -> Option<(Option<String>, String, String)> {
    let parts: Vec<&[u8]> = data.split(|&b| b == 0).collect();

    if parts.len() != 3 {
        return None;
    }

    let authzid = if parts[0].is_empty() {
        None
    } else {
        Some(std::str::from_utf8(parts[0]).ok()?.to_string())
    };

    let username = std::str::from_utf8(parts[1]).ok()?.to_string();
    let password = std::str::from_utf8(parts[2]).ok()?.to_string();

    if username.is_empty() || password.is_empty() {
        return None;
    }

    Some((authzid, username, password))
}

impl<S: PasswordCredentialStore + 'static> AuthProvider for PlainAuthProvider<S> {
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
            if method != "PLAIN" {
                debug!(method = %method, "PLAIN auth provider received non-PLAIN method");
                return Ok(EnhancedAuthResult::fail(
                    method,
                    ReasonCode::BadAuthenticationMethod,
                ));
            }

            let Some(data) = auth_data else {
                warn!("PLAIN authentication failed: no credentials provided");
                return Ok(EnhancedAuthResult::fail_with_reason(
                    method,
                    ReasonCode::NotAuthorized,
                    "No credentials provided".to_string(),
                ));
            };

            let Some((_authzid, username, password)) = parse_plain_credentials(data) else {
                warn!("PLAIN authentication failed: invalid credential format");
                return Ok(EnhancedAuthResult::fail_with_reason(
                    method,
                    ReasonCode::NotAuthorized,
                    "Invalid PLAIN credential format".to_string(),
                ));
            };

            if self.store.verify_password(&username, &password) {
                debug!(username = %username, "PLAIN authentication successful");
                Ok(EnhancedAuthResult::success_with_user(method, username))
            } else {
                warn!("PLAIN authentication failed: invalid credentials");
                Ok(EnhancedAuthResult::fail_with_reason(
                    method,
                    ReasonCode::NotAuthorized,
                    "Authentication failed".to_string(),
                ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::RwLock;

    struct MockCredentialStore {
        users: RwLock<HashMap<String, String>>,
    }

    impl MockCredentialStore {
        fn new() -> Self {
            Self {
                users: RwLock::new(HashMap::new()),
            }
        }

        fn add_user(&self, username: &str, password: &str) {
            self.users
                .write()
                .unwrap()
                .insert(username.to_string(), password.to_string());
        }
    }

    impl PasswordCredentialStore for MockCredentialStore {
        fn verify_password(&self, username: &str, password: &str) -> bool {
            self.users
                .read()
                .unwrap()
                .get(username)
                .map_or(false, |stored| stored == password)
        }
    }

    #[test]
    fn test_parse_plain_credentials_full() {
        let data = b"authzid\0username\0password";
        let result = parse_plain_credentials(data);
        assert!(result.is_some());
        let (authzid, username, password) = result.unwrap();
        assert_eq!(authzid, Some("authzid".to_string()));
        assert_eq!(username, "username");
        assert_eq!(password, "password");
    }

    #[test]
    fn test_parse_plain_credentials_no_authzid() {
        let data = b"\0username\0password";
        let result = parse_plain_credentials(data);
        assert!(result.is_some());
        let (authzid, username, password) = result.unwrap();
        assert!(authzid.is_none());
        assert_eq!(username, "username");
        assert_eq!(password, "password");
    }

    #[test]
    fn test_parse_plain_credentials_two_parts_rejected() {
        let data = b"username\0password";
        let result = parse_plain_credentials(data);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_plain_credentials_empty_username_rejected() {
        let data = b"\0\0password";
        let result = parse_plain_credentials(data);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_plain_credentials_empty_password_rejected() {
        let data = b"\0username\0";
        let result = parse_plain_credentials(data);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_plain_credentials_invalid_utf8_rejected() {
        let data = b"\0user\xFF\xFE\0password";
        let result = parse_plain_credentials(data);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_plain_auth_success() {
        let store = Arc::new(MockCredentialStore::new());
        store.add_user("testuser", "testpass");
        let provider = PlainAuthProvider::new(store);

        let data = b"\0testuser\0testpass";
        let result = provider
            .authenticate_enhanced("PLAIN", Some(data), "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Success
        );
    }

    #[tokio::test]
    async fn test_plain_auth_failure() {
        let store = Arc::new(MockCredentialStore::new());
        store.add_user("testuser", "testpass");
        let provider = PlainAuthProvider::new(store);

        let data = b"\0testuser\0wrongpass";
        let result = provider
            .authenticate_enhanced("PLAIN", Some(data), "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Failed
        );
    }

    #[tokio::test]
    async fn test_plain_auth_wrong_method() {
        let store = Arc::new(MockCredentialStore::new());
        let provider = PlainAuthProvider::new(store);

        let result = provider
            .authenticate_enhanced("JWT", None, "client-1")
            .await
            .unwrap();

        assert_eq!(
            result.status,
            crate::broker::auth::EnhancedAuthStatus::Failed
        );
        assert_eq!(result.reason_code, ReasonCode::BadAuthenticationMethod);
    }
}
