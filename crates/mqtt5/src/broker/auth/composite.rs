use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::Result;
use crate::packet::connect::ConnectPacket;
use crate::protocol::v5::reason_codes::ReasonCode;

use super::{AuthProvider, AuthResult, EnhancedAuthResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthorizationMode {
    #[default]
    PrimaryOnly,
    Or,
    And,
}

pub struct CompositeAuthProvider {
    primary: Arc<dyn AuthProvider>,
    fallback: Arc<dyn AuthProvider>,
    authorization_mode: AuthorizationMode,
}

impl CompositeAuthProvider {
    #[must_use]
    pub fn new(primary: Arc<dyn AuthProvider>, fallback: Arc<dyn AuthProvider>) -> Self {
        Self {
            primary,
            fallback,
            authorization_mode: AuthorizationMode::default(),
        }
    }

    #[must_use]
    pub fn with_authorization_mode(mut self, mode: AuthorizationMode) -> Self {
        self.authorization_mode = mode;
        self
    }
}

impl AuthProvider for CompositeAuthProvider {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        Box::pin(async move {
            let primary_result = self.primary.authenticate(connect, client_addr).await?;
            if primary_result.authenticated {
                return Ok(primary_result);
            }
            if primary_result.reason_code == ReasonCode::BadAuthenticationMethod {
                return self.fallback.authenticate(connect, client_addr).await;
            }
            Ok(primary_result)
        })
    }

    fn authorize_publish<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        let client_id = client_id.to_string();
        let mode = self.authorization_mode;
        Box::pin(async move {
            let primary = self
                .primary
                .authorize_publish(&client_id, user_id, topic)
                .await;
            match mode {
                AuthorizationMode::PrimaryOnly => primary,
                AuthorizationMode::Or => {
                    primary
                        || self
                            .fallback
                            .authorize_publish(&client_id, user_id, topic)
                            .await
                }
                AuthorizationMode::And => {
                    primary
                        && self
                            .fallback
                            .authorize_publish(&client_id, user_id, topic)
                            .await
                }
            }
        })
    }

    fn authorize_subscribe<'a>(
        &'a self,
        client_id: &str,
        user_id: Option<&'a str>,
        topic_filter: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        let client_id = client_id.to_string();
        let mode = self.authorization_mode;
        Box::pin(async move {
            let primary = self
                .primary
                .authorize_subscribe(&client_id, user_id, topic_filter)
                .await;
            match mode {
                AuthorizationMode::PrimaryOnly => primary,
                AuthorizationMode::Or => {
                    primary
                        || self
                            .fallback
                            .authorize_subscribe(&client_id, user_id, topic_filter)
                            .await
                }
                AuthorizationMode::And => {
                    primary
                        && self
                            .fallback
                            .authorize_subscribe(&client_id, user_id, topic_filter)
                            .await
                }
            }
        })
    }

    fn supports_enhanced_auth(&self) -> bool {
        self.primary.supports_enhanced_auth()
    }

    fn authenticate_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        self.primary
            .authenticate_enhanced(auth_method, auth_data, client_id)
    }

    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
        user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        self.primary
            .reauthenticate(auth_method, auth_data, client_id, user_id)
    }

    fn cleanup_session<'a>(
        &'a self,
        user_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            self.primary.cleanup_session(user_id).await;
            self.fallback.cleanup_session(user_id).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broker::auth::AllowAllAuthProvider;

    struct RejectWithBadAuthMethod;

    impl AuthProvider for RejectWithBadAuthMethod {
        fn authenticate<'a>(
            &'a self,
            _connect: &'a ConnectPacket,
            _client_addr: SocketAddr,
        ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
            Box::pin(async { Ok(AuthResult::fail(ReasonCode::BadAuthenticationMethod)) })
        }

        fn authorize_publish<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async { false })
        }

        fn authorize_subscribe<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic_filter: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async { false })
        }
    }

    struct RejectWithNotAuthorized;

    impl AuthProvider for RejectWithNotAuthorized {
        fn authenticate<'a>(
            &'a self,
            _connect: &'a ConnectPacket,
            _client_addr: SocketAddr,
        ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
            Box::pin(async { Ok(AuthResult::fail(ReasonCode::NotAuthorized)) })
        }

        fn authorize_publish<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async { false })
        }

        fn authorize_subscribe<'a>(
            &'a self,
            _client_id: &str,
            _user_id: Option<&'a str>,
            _topic_filter: &'a str,
        ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async { false })
        }
    }

    #[tokio::test]
    async fn primary_success_skips_fallback() {
        let composite = CompositeAuthProvider::new(
            Arc::new(AllowAllAuthProvider),
            Arc::new(RejectWithNotAuthorized),
        );
        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("test"));
        let addr = "127.0.0.1:1234".parse().unwrap();

        let result = composite.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
    }

    #[tokio::test]
    async fn bad_auth_method_falls_through_to_fallback() {
        let composite = CompositeAuthProvider::new(
            Arc::new(RejectWithBadAuthMethod),
            Arc::new(AllowAllAuthProvider),
        );
        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("test"));
        let addr = "127.0.0.1:1234".parse().unwrap();

        let result = composite.authenticate(&connect, addr).await.unwrap();
        assert!(result.authenticated);
    }

    #[tokio::test]
    async fn non_bad_auth_rejection_does_not_fall_through() {
        let composite = CompositeAuthProvider::new(
            Arc::new(RejectWithNotAuthorized),
            Arc::new(AllowAllAuthProvider),
        );
        let connect = ConnectPacket::new(mqtt5_protocol::ConnectOptions::new("test"));
        let addr = "127.0.0.1:1234".parse().unwrap();

        let result = composite.authenticate(&connect, addr).await.unwrap();
        assert!(!result.authenticated);
        assert_eq!(result.reason_code, ReasonCode::NotAuthorized);
    }

    #[tokio::test]
    async fn authorize_publish_or_mode_falls_through() {
        let composite = CompositeAuthProvider::new(
            Arc::new(RejectWithBadAuthMethod),
            Arc::new(AllowAllAuthProvider),
        )
        .with_authorization_mode(AuthorizationMode::Or);

        let result = composite
            .authorize_publish("client", Some("user"), "topic")
            .await;
        assert!(result);
    }

    #[tokio::test]
    async fn authorize_subscribe_or_mode_falls_through() {
        let composite = CompositeAuthProvider::new(
            Arc::new(RejectWithBadAuthMethod),
            Arc::new(AllowAllAuthProvider),
        )
        .with_authorization_mode(AuthorizationMode::Or);

        let result = composite
            .authorize_subscribe("client", Some("user"), "topic/#")
            .await;
        assert!(result);
    }

    #[tokio::test]
    async fn authorize_publish_primary_only_ignores_fallback() {
        let composite = CompositeAuthProvider::new(
            Arc::new(RejectWithBadAuthMethod),
            Arc::new(AllowAllAuthProvider),
        );

        let result = composite
            .authorize_publish("client", Some("user"), "topic")
            .await;
        assert!(!result);
    }

    #[tokio::test]
    async fn authorize_subscribe_primary_only_ignores_fallback() {
        let composite = CompositeAuthProvider::new(
            Arc::new(RejectWithBadAuthMethod),
            Arc::new(AllowAllAuthProvider),
        );

        let result = composite
            .authorize_subscribe("client", Some("user"), "topic/#")
            .await;
        assert!(!result);
    }

    #[tokio::test]
    async fn authorize_publish_and_mode_requires_both() {
        let both_allow = CompositeAuthProvider::new(
            Arc::new(AllowAllAuthProvider),
            Arc::new(AllowAllAuthProvider),
        )
        .with_authorization_mode(AuthorizationMode::And);
        assert!(
            both_allow
                .authorize_publish("client", Some("user"), "topic")
                .await
        );

        let primary_rejects = CompositeAuthProvider::new(
            Arc::new(RejectWithBadAuthMethod),
            Arc::new(AllowAllAuthProvider),
        )
        .with_authorization_mode(AuthorizationMode::And);
        assert!(
            !primary_rejects
                .authorize_publish("client", Some("user"), "topic")
                .await
        );
    }

    #[tokio::test]
    async fn authorize_subscribe_and_mode_requires_both() {
        let both_allow = CompositeAuthProvider::new(
            Arc::new(AllowAllAuthProvider),
            Arc::new(AllowAllAuthProvider),
        )
        .with_authorization_mode(AuthorizationMode::And);
        assert!(
            both_allow
                .authorize_subscribe("client", Some("user"), "topic/#")
                .await
        );

        let fallback_rejects = CompositeAuthProvider::new(
            Arc::new(AllowAllAuthProvider),
            Arc::new(RejectWithNotAuthorized),
        )
        .with_authorization_mode(AuthorizationMode::And);
        assert!(
            !fallback_rejects
                .authorize_subscribe("client", Some("user"), "topic/#")
                .await
        );
    }

    #[tokio::test]
    async fn cleanup_calls_both() {
        let composite = CompositeAuthProvider::new(
            Arc::new(AllowAllAuthProvider),
            Arc::new(AllowAllAuthProvider),
        );
        composite.cleanup_session("user").await;
    }

    #[test]
    fn supports_enhanced_auth_delegates_to_primary() {
        let composite = CompositeAuthProvider::new(
            Arc::new(AllowAllAuthProvider),
            Arc::new(AllowAllAuthProvider),
        );
        assert!(!composite.supports_enhanced_auth());
    }

    #[test]
    fn default_authorization_mode_is_primary_only() {
        let composite = CompositeAuthProvider::new(
            Arc::new(AllowAllAuthProvider),
            Arc::new(AllowAllAuthProvider),
        );
        assert_eq!(composite.authorization_mode, AuthorizationMode::PrimaryOnly);
    }
}
