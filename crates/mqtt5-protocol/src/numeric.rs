pub const F64_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[must_use]
pub fn u128_to_u64_saturating(value: u128) -> u64 {
    if value <= u128::from(u64::MAX) {
        #[allow(clippy::cast_possible_truncation)]
        let result = value as u64;
        result
    } else {
        u64::MAX
    }
}

#[must_use]
pub fn u128_to_f64_saturating(value: u128) -> f64 {
    if value <= u128::from(F64_MAX_SAFE_INTEGER) {
        #[allow(clippy::cast_precision_loss)]
        let result = value as f64;
        result
    } else {
        f64::MAX
    }
}

#[must_use]
pub fn usize_to_f64_saturating(value: usize) -> f64 {
    u64_to_f64_saturating(value as u64)
}

#[must_use]
pub fn u64_to_f64_saturating(value: u64) -> f64 {
    if value <= F64_MAX_SAFE_INTEGER {
        #[allow(clippy::cast_precision_loss)]
        let result = value as f64;
        result
    } else {
        f64::MAX
    }
}

#[must_use]
pub fn u128_to_u32_saturating(value: u128) -> u32 {
    if value <= u128::from(u32::MAX) {
        #[allow(clippy::cast_possible_truncation)]
        let result = value as u32;
        result
    } else {
        u32::MAX
    }
}

#[must_use]
pub fn usize_to_u32_saturating(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[must_use]
pub fn u64_to_u32_saturating(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[must_use]
pub fn u64_to_u16_saturating(value: u64) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[must_use]
pub fn usize_to_u16_saturating(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[must_use]
pub fn i32_to_u32_saturating(value: i32) -> u32 {
    if value < 0 {
        0
    } else {
        #[allow(clippy::cast_sign_loss)]
        let result = value as u32;
        result
    }
}

#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn f64_to_u64_saturating(value: f64) -> u64 {
    const U64_MAX_AS_F64: f64 = u64::MAX as f64;
    if value < 0.0 {
        0
    } else if value >= U64_MAX_AS_F64 {
        u64::MAX
    } else {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let result = value as u64;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_u128_to_u64_saturating() {
        assert_eq!(u128_to_u64_saturating(0), 0);
        assert_eq!(u128_to_u64_saturating(1000), 1000);
        assert_eq!(u128_to_u64_saturating(u128::from(u64::MAX)), u64::MAX);
        assert_eq!(u128_to_u64_saturating(u128::MAX), u64::MAX);
    }

    #[test]
    fn test_u128_to_f64_saturating() {
        assert!((u128_to_f64_saturating(0) - 0.0).abs() < f64::EPSILON);
        assert!((u128_to_f64_saturating(1000) - 1000.0).abs() < f64::EPSILON);
        assert!((u128_to_f64_saturating(u128::MAX) - f64::MAX).abs() < f64::EPSILON);
    }

    #[test]
    fn test_usize_to_f64_saturating() {
        assert!((usize_to_f64_saturating(0) - 0.0).abs() < f64::EPSILON);
        assert!((usize_to_f64_saturating(1000) - 1000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_usize_to_u32_saturating() {
        assert_eq!(usize_to_u32_saturating(0), 0);
        assert_eq!(usize_to_u32_saturating(1000), 1000);
        assert_eq!(usize_to_u32_saturating(u32::MAX as usize), u32::MAX);
    }

    #[test]
    fn test_u64_to_u16_saturating() {
        assert_eq!(u64_to_u16_saturating(0), 0);
        assert_eq!(u64_to_u16_saturating(1000), 1000);
        assert_eq!(u64_to_u16_saturating(u64::from(u16::MAX)), u16::MAX);
        assert_eq!(u64_to_u16_saturating(u64::MAX), u16::MAX);
    }

    #[test]
    fn test_i32_to_u32_saturating() {
        assert_eq!(i32_to_u32_saturating(-100), 0);
        assert_eq!(i32_to_u32_saturating(0), 0);
        assert_eq!(i32_to_u32_saturating(1000), 1000);
        #[allow(clippy::cast_sign_loss)]
        let expected = i32::MAX as u32;
        assert_eq!(i32_to_u32_saturating(i32::MAX), expected);
    }
}
