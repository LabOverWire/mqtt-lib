use super::{AuthProvider, AuthResult, EnhancedAuthResult};
use crate::broker::acl::AclManager;
use crate::error::Result;
use crate::packet::connect::ConnectPacket;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;

/// Authenticates with one provider, such as SCRAM or JWT, and authorizes every publish,
/// subscription and delivery with an ACL.
pub(crate) struct AclAuthorizedProvider {
    authenticator: Arc<dyn AuthProvider>,
    acl_manager: AclManager,
}

impl AclAuthorizedProvider {
    pub(crate) fn new(authenticator: Arc<dyn AuthProvider>, acl_manager: AclManager) -> Self {
        Self {
            authenticator,
            acl_manager,
        }
    }
}

impl AuthProvider for AclAuthorizedProvider {
    fn authenticate<'a>(
        &'a self,
        connect: &'a ConnectPacket,
        client_addr: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = Result<AuthResult>> + Send + 'a>> {
        self.authenticator.authenticate(connect, client_addr)
    }

    fn authorize_publish<'a>(
        &'a self,
        _client_id: &str,
        user_id: Option<&'a str>,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { self.acl_manager.check_publish(user_id, topic).await })
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

    fn checks_delivery(&self) -> bool {
        true
    }

    fn authorize_delivery<'a>(
        &'a self,
        _client_id: &str,
        user_id: Option<&'a str>,
        topic: &'a str,
    ) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { self.acl_manager.check_delivery(user_id, topic).await })
    }

    fn supports_enhanced_auth(&self) -> bool {
        self.authenticator.supports_enhanced_auth()
    }

    fn authenticate_enhanced<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        self.authenticator
            .authenticate_enhanced(auth_method, auth_data, client_id)
    }

    fn reauthenticate<'a>(
        &'a self,
        auth_method: &'a str,
        auth_data: Option<&'a [u8]>,
        client_id: &'a str,
        user_id: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<EnhancedAuthResult>> + Send + 'a>> {
        self.authenticator
            .reauthenticate(auth_method, auth_data, client_id, user_id)
    }

    fn cleanup_session<'a>(
        &'a self,
        user_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        self.authenticator.cleanup_session(user_id)
    }
}
