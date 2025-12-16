use crate::client::auth_handler::{AuthFuture, AuthHandler, AuthResponse};
use base64::prelude::*;
use ring::{hmac, pbkdf2};
use sha2::{Digest, Sha256};
use std::num::NonZeroU32;
use std::sync::Mutex;
use tracing::debug;

enum ScramClientState {
    Initial,
    WaitingForServerFirst {
        client_nonce: String,
        client_first_bare: String,
    },
    WaitingForServerFinal {
        expected_server_signature: [u8; 32],
    },
}

pub struct ScramSha256AuthHandler {
    username: String,
    password: String,
    state: Mutex<ScramClientState>,
}

impl ScramSha256AuthHandler {
    #[must_use]
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            username: username.into(),
            password: password.into(),
            state: Mutex::new(ScramClientState::Initial),
        }
    }

    fn generate_nonce() -> String {
        let mut bytes = [0u8; 24];
        getrandom::fill(&mut bytes).expect("failed to generate random nonce");
        BASE64_STANDARD.encode(bytes)
    }
}

impl AuthHandler for ScramSha256AuthHandler {
    fn initial_response<'a>(&'a self, _auth_method: &'a str) -> AuthFuture<'a, Option<Vec<u8>>> {
        let username = self.username.clone();

        Box::pin(async move {
            let client_nonce = Self::generate_nonce();
            let client_first_bare = format!("n={username},r={client_nonce}");
            let client_first = format!("n,,{client_first_bare}");

            *self.state.lock().unwrap() = ScramClientState::WaitingForServerFirst {
                client_nonce,
                client_first_bare,
            };

            debug!("SCRAM client: sending client-first message");
            Ok(Some(client_first.into_bytes()))
        })
    }

    fn handle_challenge<'a>(
        &'a self,
        _auth_method: &'a str,
        challenge_data: Option<&'a [u8]>,
    ) -> AuthFuture<'a, AuthResponse> {
        let password = self.password.clone();

        Box::pin(async move {
            let Some(data) = challenge_data else {
                return Ok(AuthResponse::Abort(
                    "No challenge data received".to_string(),
                ));
            };

            let server_message = String::from_utf8_lossy(data);

            let state =
                std::mem::replace(&mut *self.state.lock().unwrap(), ScramClientState::Initial);

            match state {
                ScramClientState::WaitingForServerFirst {
                    client_nonce,
                    client_first_bare,
                } => {
                    let attrs = parse_scram_attributes(&server_message);

                    let Some(combined_nonce) = attrs.get("r") else {
                        return Ok(AuthResponse::Abort("Missing nonce in server-first".into()));
                    };

                    if !combined_nonce.starts_with(&client_nonce) {
                        return Ok(AuthResponse::Abort(
                            "Server nonce doesn't contain client nonce".into(),
                        ));
                    }

                    let Some(salt_b64) = attrs.get("s") else {
                        return Ok(AuthResponse::Abort("Missing salt in server-first".into()));
                    };

                    let Ok(salt) = BASE64_STANDARD.decode(salt_b64) else {
                        return Ok(AuthResponse::Abort("Invalid salt encoding".into()));
                    };

                    let Some(iterations_str) = attrs.get("i") else {
                        return Ok(AuthResponse::Abort(
                            "Missing iteration count in server-first".into(),
                        ));
                    };

                    let Ok(iterations) = iterations_str.parse::<u32>() else {
                        return Ok(AuthResponse::Abort("Invalid iteration count".into()));
                    };

                    if iterations < 4096 {
                        return Ok(AuthResponse::Abort("Iteration count too low".into()));
                    }

                    let salted_password = pbkdf2_sha256(password.as_bytes(), &salt, iterations);
                    let client_key = hmac_sha256(&salted_password, b"Client Key");
                    let stored_key = sha256(&client_key);
                    let server_key = hmac_sha256(&salted_password, b"Server Key");

                    let client_final_without_proof = format!("c=biws,r={combined_nonce}");
                    let auth_message = format!(
                        "{client_first_bare},{server_message},{client_final_without_proof}"
                    );

                    let client_signature = hmac_sha256(&stored_key, auth_message.as_bytes());
                    let client_proof: Vec<u8> = client_key
                        .iter()
                        .zip(client_signature.iter())
                        .map(|(a, b)| a ^ b)
                        .collect();

                    let expected_server_signature =
                        hmac_sha256(&server_key, auth_message.as_bytes());

                    *self.state.lock().unwrap() = ScramClientState::WaitingForServerFinal {
                        expected_server_signature,
                    };

                    let proof_b64 = BASE64_STANDARD.encode(&client_proof);
                    let client_final = format!("{client_final_without_proof},p={proof_b64}");

                    debug!("SCRAM client: sending client-final message");
                    Ok(AuthResponse::Continue(client_final.into_bytes()))
                }
                ScramClientState::WaitingForServerFinal {
                    expected_server_signature,
                } => {
                    let attrs = parse_scram_attributes(&server_message);

                    let Some(verifier_b64) = attrs.get("v") else {
                        return Ok(AuthResponse::Abort(
                            "Missing verifier in server-final".into(),
                        ));
                    };

                    let Ok(received_signature) = BASE64_STANDARD.decode(verifier_b64) else {
                        return Ok(AuthResponse::Abort("Invalid verifier encoding".into()));
                    };

                    if received_signature.len() != 32 {
                        return Ok(AuthResponse::Abort("Invalid verifier length".into()));
                    }

                    if !constant_time_compare(&received_signature, &expected_server_signature) {
                        return Ok(AuthResponse::Abort(
                            "Server signature verification failed".into(),
                        ));
                    }

                    debug!("SCRAM client: server signature verified, authentication complete");
                    Ok(AuthResponse::Success)
                }
                ScramClientState::Initial => {
                    Ok(AuthResponse::Abort("Invalid state: Initial".into()))
                }
            }
        })
    }
}

fn constant_time_compare(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

fn parse_scram_attributes(message: &str) -> std::collections::HashMap<String, String> {
    let mut attrs = std::collections::HashMap::new();
    for part in message.split(',') {
        if let Some((key, value)) = part.split_once('=') {
            attrs.insert(key.to_string(), value.to_string());
        }
    }
    attrs
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(iterations).expect("iterations must be non-zero"),
        salt,
        password,
        &mut out,
    );
    out
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&key, data);
    let mut result = [0u8; 32];
    result.copy_from_slice(tag.as_ref());
    result
}

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scram_initial_response() {
        let handler = ScramSha256AuthHandler::new("testuser", "testpass");
        let response = handler.initial_response("SCRAM-SHA-256").await.unwrap();

        assert!(response.is_some());
        let msg = String::from_utf8_lossy(response.as_ref().unwrap());
        assert!(msg.starts_with("n,,n=testuser,r="));
    }

    #[tokio::test]
    async fn test_scram_handle_server_first() {
        let handler = ScramSha256AuthHandler::new("testuser", "testpass");

        let _ = handler.initial_response("SCRAM-SHA-256").await.unwrap();

        let client_nonce = match &*handler.state.lock().unwrap() {
            ScramClientState::WaitingForServerFirst { client_nonce, .. } => client_nonce.clone(),
            _ => panic!("Wrong state"),
        };

        let server_first = format!(
            "r={client_nonce}servernonce,s={},i=4096",
            BASE64_STANDARD.encode(b"testsalt")
        );

        let response = handler
            .handle_challenge("SCRAM-SHA-256", Some(server_first.as_bytes()))
            .await
            .unwrap();

        match response {
            AuthResponse::Continue(data) => {
                let msg = String::from_utf8_lossy(&data);
                assert!(msg.starts_with("c=biws,r="));
                assert!(msg.contains(",p="));
            }
            other => panic!("Expected Continue, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_scram_handle_server_final_valid() {
        let handler = ScramSha256AuthHandler::new("testuser", "testpass");

        let expected_sig = [0u8; 32];
        *handler.state.lock().unwrap() = ScramClientState::WaitingForServerFinal {
            expected_server_signature: expected_sig,
        };

        let server_final = format!("v={}", BASE64_STANDARD.encode(expected_sig));
        let response = handler
            .handle_challenge("SCRAM-SHA-256", Some(server_final.as_bytes()))
            .await
            .unwrap();

        assert!(matches!(response, AuthResponse::Success));
    }

    #[tokio::test]
    async fn test_scram_handle_server_final_invalid() {
        let handler = ScramSha256AuthHandler::new("testuser", "testpass");

        let expected_sig = [0u8; 32];
        let wrong_sig = [1u8; 32];
        *handler.state.lock().unwrap() = ScramClientState::WaitingForServerFinal {
            expected_server_signature: expected_sig,
        };

        let server_final = format!("v={}", BASE64_STANDARD.encode(wrong_sig));
        let response = handler
            .handle_challenge("SCRAM-SHA-256", Some(server_final.as_bytes()))
            .await
            .unwrap();

        match response {
            AuthResponse::Abort(msg) => {
                assert!(msg.contains("Server signature verification failed"));
            }
            other => panic!("Expected Abort, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_scram_nonce_mismatch() {
        let handler = ScramSha256AuthHandler::new("testuser", "testpass");

        let _ = handler.initial_response("SCRAM-SHA-256").await.unwrap();

        let server_first = format!(
            "r=differentnonce,s={},i=4096",
            BASE64_STANDARD.encode(b"testsalt")
        );

        let response = handler
            .handle_challenge("SCRAM-SHA-256", Some(server_first.as_bytes()))
            .await
            .unwrap();

        match response {
            AuthResponse::Abort(msg) => {
                assert!(msg.contains("nonce"));
            }
            other => panic!("Expected Abort, got {:?}", other),
        }
    }
}
