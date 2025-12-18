use crate::client::auth_handler::{AuthFuture, AuthHandler, AuthResponse};

pub struct PlainAuthHandler {
    username: String,
    password: String,
    authzid: Option<String>,
}

impl PlainAuthHandler {
    #[must_use]
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            authzid: None,
        }
    }

    #[must_use]
    pub fn with_authzid(mut self, authzid: impl Into<String>) -> Self {
        self.authzid = Some(authzid.into());
        self
    }
}

impl AuthHandler for PlainAuthHandler {
    fn initial_response<'a>(&'a self, _auth_method: &'a str) -> AuthFuture<'a, Option<Vec<u8>>> {
        let authzid = self.authzid.clone();
        let username = self.username.clone();
        let password = self.password.clone();

        Box::pin(async move {
            let mut data = Vec::new();
            if let Some(az) = authzid {
                data.extend_from_slice(az.as_bytes());
            }
            data.push(0);
            data.extend_from_slice(username.as_bytes());
            data.push(0);
            data.extend_from_slice(password.as_bytes());
            Ok(Some(data))
        })
    }

    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        _challenge_data: Option<&'a [u8]>,
    ) -> AuthFuture<'a, AuthResponse> {
        Box::pin(async move { Ok(AuthResponse::Success) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_plain_handler_no_authzid() {
        let handler = PlainAuthHandler::new("user", "pass");
        let response = handler.initial_response("PLAIN").await.unwrap();
        assert_eq!(response, Some(b"\0user\0pass".to_vec()));
    }

    #[tokio::test]
    async fn test_plain_handler_with_authzid() {
        let handler = PlainAuthHandler::new("user", "pass").with_authzid("admin");
        let response = handler.initial_response("PLAIN").await.unwrap();
        assert_eq!(response, Some(b"admin\0user\0pass".to_vec()));
    }

    #[tokio::test]
    async fn test_plain_handler_challenge_returns_success() {
        let handler = PlainAuthHandler::new("user", "pass");
        let response = handler.handle_challenge("PLAIN", None).await.unwrap();
        assert!(matches!(response, AuthResponse::Success));
    }
}
