use crate::error::Result;
use std::future::Future;
use std::pin::Pin;

#[derive(Debug, Clone)]
pub enum AuthResponse {
    Continue(Vec<u8>),
    Success,
    Abort(String),
}

pub type AuthFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;

pub trait AuthHandler: Send + Sync {
    fn handle_challenge<'a>(
        &'a self,
        auth_method: &'a str,
        challenge_data: Option<&'a [u8]>,
    ) -> AuthFuture<'a, AuthResponse>;

    fn initial_response<'a>(&'a self, _auth_method: &'a str) -> AuthFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move { Ok(None) })
    }
}
