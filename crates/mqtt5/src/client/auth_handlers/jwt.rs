use crate::client::auth_handler::{AuthFuture, AuthHandler, AuthResponse};

pub struct JwtAuthHandler {
    token: String,
}

impl JwtAuthHandler {
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

impl AuthHandler for JwtAuthHandler {
    fn initial_response<'a>(&'a self, _auth_method: &'a str) -> AuthFuture<'a, Option<Vec<u8>>> {
        let token = self.token.clone();
        Box::pin(async move { Ok(Some(token.into_bytes())) })
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
    async fn test_jwt_handler_initial_response() {
        let handler = JwtAuthHandler::new("my.jwt.token");
        let response = handler.initial_response("JWT").await.unwrap();
        assert_eq!(response, Some(b"my.jwt.token".to_vec()));
    }

    #[tokio::test]
    async fn test_jwt_handler_challenge_returns_success() {
        let handler = JwtAuthHandler::new("my.jwt.token");
        let response = handler.handle_challenge("JWT", None).await.unwrap();
        assert!(matches!(response, AuthResponse::Success));
    }
}
