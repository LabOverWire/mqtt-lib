mod verifiers;

use rustls::crypto::CryptoProvider;
use std::sync::Arc;

pub(crate) use verifiers::NoVerification;

pub(crate) fn ring_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}
