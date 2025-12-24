#[cfg(all(feature = "std", not(target_arch = "wasm32")))]
pub use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(target_arch = "wasm32")]
pub use web_time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(all(not(feature = "std"), not(target_arch = "wasm32")))]
mod embedded {
    use portable_atomic::{AtomicU32, Ordering};

    static TIME_SOURCE_MILLIS_LO: AtomicU32 = AtomicU32::new(0);
    static TIME_SOURCE_MILLIS_HI: AtomicU32 = AtomicU32::new(0);
    static EPOCH_MILLIS_LO: AtomicU32 = AtomicU32::new(0);
    static EPOCH_MILLIS_HI: AtomicU32 = AtomicU32::new(0);

    #[allow(clippy::cast_possible_truncation)]
    pub fn set_time_source(monotonic_millis: u64, epoch_millis: u64) {
        TIME_SOURCE_MILLIS_LO.store(monotonic_millis as u32, Ordering::SeqCst);
        TIME_SOURCE_MILLIS_HI.store((monotonic_millis >> 32) as u32, Ordering::SeqCst);
        EPOCH_MILLIS_LO.store(epoch_millis as u32, Ordering::SeqCst);
        EPOCH_MILLIS_HI.store((epoch_millis >> 32) as u32, Ordering::SeqCst);
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn update_monotonic_time(millis: u64) {
        TIME_SOURCE_MILLIS_LO.store(millis as u32, Ordering::SeqCst);
        TIME_SOURCE_MILLIS_HI.store((millis >> 32) as u32, Ordering::SeqCst);
    }

    #[allow(clippy::cast_possible_truncation)]
    pub fn update_epoch_time(millis_since_epoch: u64) {
        EPOCH_MILLIS_LO.store(millis_since_epoch as u32, Ordering::SeqCst);
        EPOCH_MILLIS_HI.store((millis_since_epoch >> 32) as u32, Ordering::SeqCst);
    }

    fn get_monotonic_millis() -> u64 {
        let lo = u64::from(TIME_SOURCE_MILLIS_LO.load(Ordering::SeqCst));
        let hi = u64::from(TIME_SOURCE_MILLIS_HI.load(Ordering::SeqCst));
        (hi << 32) | lo
    }

    fn get_epoch_millis() -> u64 {
        let lo = u64::from(EPOCH_MILLIS_LO.load(Ordering::SeqCst));
        let hi = u64::from(EPOCH_MILLIS_HI.load(Ordering::SeqCst));
        (hi << 32) | lo
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct Duration {
        millis: u64,
    }

    impl Duration {
        #[must_use]
        pub const fn from_secs(secs: u64) -> Self {
            Self {
                millis: secs.saturating_mul(1000),
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
                millis: self.millis.saturating_add(rhs.millis),
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
            Self {
                millis: get_monotonic_millis(),
            }
        }

        #[must_use]
        pub fn elapsed(&self) -> Duration {
            let now = get_monotonic_millis();
            Duration::from_millis(now.saturating_sub(self.millis))
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
                millis: self.millis.saturating_add(rhs.millis),
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
                millis_since_epoch: get_epoch_millis(),
            }
        }

        /// Returns the duration since an earlier `SystemTime`.
        ///
        /// # Errors
        ///
        /// Returns `TimeError::EarlierTime` if `earlier` is after `self`.
        pub fn duration_since(&self, earlier: Self) -> Result<Duration, TimeError> {
            if self.millis_since_epoch >= earlier.millis_since_epoch {
                Ok(Duration::from_millis(
                    self.millis_since_epoch - earlier.millis_since_epoch,
                ))
            } else {
                Err(TimeError::EarlierTime)
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum TimeError {
        EarlierTime,
    }

    pub const UNIX_EPOCH: SystemTime = SystemTime {
        millis_since_epoch: 0,
    };
}

#[cfg(all(not(feature = "std"), not(target_arch = "wasm32")))]
pub use embedded::{
    set_time_source, update_epoch_time, update_monotonic_time, Duration, Instant, SystemTime,
    TimeError, UNIX_EPOCH,
};

#[must_use]
pub fn is_using_web_time() -> bool {
    cfg!(target_arch = "wasm32")
}
