#[cfg(feature = "std")]
pub use std::{
    boxed::Box,
    collections::{HashMap, VecDeque},
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

#[cfg(not(feature = "std"))]
pub use alloc::{
    boxed::Box,
    collections::VecDeque,
    format,
    string::{String, ToString},
    vec,
    vec::Vec,
};

#[cfg(not(feature = "std"))]
pub use hashbrown::HashMap;

#[cfg(feature = "tracing")]
macro_rules! warn_log {
    ($($arg:tt)*) => { tracing::warn!($($arg)*) };
}

#[cfg(not(feature = "tracing"))]
macro_rules! warn_log {
    ($($arg:tt)*) => {};
}

pub(crate) use warn_log;
