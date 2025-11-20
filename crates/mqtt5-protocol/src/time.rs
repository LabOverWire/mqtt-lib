#[cfg(not(target_arch = "wasm32"))]
pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_arch = "wasm32")]
pub use web_time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[must_use]
pub fn is_using_web_time() -> bool {
    cfg!(target_arch = "wasm32")
}
