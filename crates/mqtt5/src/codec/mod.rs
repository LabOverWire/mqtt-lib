mod registry;

#[cfg(feature = "codec-gzip")]
mod gzip;

#[cfg(feature = "codec-deflate")]
mod deflate;

pub use registry::CodecRegistry;

#[cfg(feature = "codec-gzip")]
pub use gzip::GzipCodec;

#[cfg(feature = "codec-deflate")]
pub use deflate::DeflateCodec;

use crate::error::Result;
use bytes::Bytes;

pub trait PayloadCodec: Send + Sync {
    fn name(&self) -> &'static str;

    fn content_type(&self) -> &'static str;

    /// # Errors
    /// Returns an error if encoding fails
    fn encode(&self, payload: &[u8]) -> Result<Bytes>;

    /// # Errors
    /// Returns an error if decoding fails
    fn decode(&self, payload: &[u8]) -> Result<Bytes>;

    fn should_encode(&self, payload: &[u8]) -> bool {
        payload.len() >= self.min_size_threshold()
    }

    fn min_size_threshold(&self) -> usize {
        128
    }
}
