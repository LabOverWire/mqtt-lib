pub fn u128_to_f64_saturating(value: u128) -> f64 {
    if value <= js_sys::Number::MAX_SAFE_INTEGER as u128 {
        value as f64
    } else {
        f64::MAX
    }
}

pub fn usize_to_f64_saturating(value: usize) -> f64 {
    if value <= js_sys::Number::MAX_SAFE_INTEGER as usize {
        value as f64
    } else {
        f64::MAX
    }
}
