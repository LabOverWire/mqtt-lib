#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_arch = "wasm32")]
pub use web_time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(all(not(feature = "std"), not(target_arch = "wasm32")))]
mod embedded {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Duration {
        millis: u64,
    }

    impl Duration {
        #[must_use]
        pub const fn from_secs(secs: u64) -> Self {
            Self {
                millis: secs * 1000,
            }
        }

        #[must_use]
        pub const fn from_millis(millis: u64) -> Self {
            Self { millis }
        }

        #[must_use]
        pub const fn as_secs(&self) -> u64 {
            self.millis / 1000
        }

        #[must_use]
        pub const fn as_millis(&self) -> u128 {
            self.millis as u128
        }

        #[must_use]
        pub const fn subsec_millis(&self) -> u32 {
            (self.millis % 1000) as u32
        }

        #[must_use]
        pub fn min(self, other: Self) -> Self {
            if self.millis < other.millis {
                self
            } else {
                other
            }
        }

        #[must_use]
        pub const fn is_zero(&self) -> bool {
            self.millis == 0
        }
    }

    impl core::ops::Add for Duration {
        type Output = Self;

        fn add(self, rhs: Self) -> Self::Output {
            Self {
                millis: self.millis + rhs.millis,
            }
        }
    }

    impl core::ops::Sub for Duration {
        type Output = Self;

        fn sub(self, rhs: Self) -> Self::Output {
            Self {
                millis: self.millis.saturating_sub(rhs.millis),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Instant {
        millis: u64,
    }

    impl Instant {
        #[must_use]
        pub fn now() -> Self {
            Self { millis: 0 }
        }

        #[must_use]
        pub fn elapsed(&self) -> Duration {
            Duration::from_millis(0)
        }

        #[must_use]
        pub fn duration_since(&self, earlier: Self) -> Duration {
            Duration::from_millis(self.millis.saturating_sub(earlier.millis))
        }

        #[must_use]
        pub fn checked_sub(&self, duration: Duration) -> Option<Self> {
            self.millis
                .checked_sub(duration.millis)
                .map(|millis| Self { millis })
        }
    }

    impl core::ops::Add<Duration> for Instant {
        type Output = Self;

        fn add(self, rhs: Duration) -> Self::Output {
            Self {
                millis: self.millis + rhs.millis,
            }
        }
    }

    impl core::ops::Sub<Duration> for Instant {
        type Output = Self;

        fn sub(self, rhs: Duration) -> Self::Output {
            Self {
                millis: self.millis.saturating_sub(rhs.millis),
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SystemTime {
        millis_since_epoch: u64,
    }

    impl SystemTime {
        #[must_use]
        pub fn now() -> Self {
            Self {
                millis_since_epoch: 0,
            }
        }

        #[allow(clippy::result_unit_err, clippy::missing_errors_doc)]
        pub fn duration_since(&self, earlier: Self) -> Result<Duration, ()> {
            if self.millis_since_epoch >= earlier.millis_since_epoch {
                Ok(Duration::from_millis(
                    self.millis_since_epoch - earlier.millis_since_epoch,
                ))
            } else {
                Err(())
            }
        }
    }

    pub const UNIX_EPOCH: SystemTime = SystemTime {
        millis_since_epoch: 0,
    };
}

#[cfg(all(not(feature = "std"), not(target_arch = "wasm32")))]
pub use embedded::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[must_use]
pub fn is_using_web_time() -> bool {
    cfg!(target_arch = "wasm32")
}
