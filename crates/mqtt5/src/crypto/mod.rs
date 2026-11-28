mod verifiers;

use rustls::crypto::CryptoProvider;
use std::sync::Arc;

pub(crate) use verifiers::NoVerification;

pub(crate) fn crypto_provider() -> Arc<CryptoProvider> {
    CryptoProvider::get_default().map_or_else(
        || Arc::new(rustls::crypto::ring::default_provider()),
        Arc::clone,
    )
}
