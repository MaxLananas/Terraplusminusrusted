//! Java `Math`/PorkLib `PMath` semantics that differ from Rust's standard library.

/// `java.lang.Math.min(double, double)`: NaN propagates and `-0.0 < 0.0`.
pub fn java_min(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        a
    } else if b.is_nan() {
        b
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_negative() { a } else { b }
    } else if a <= b {
        a
    } else {
        b
    }
}

/// `java.lang.Math.max(double, double)`: NaN propagates and `0.0 > -0.0`.
pub fn java_max(a: f64, b: f64) -> f64 {
    if a.is_nan() {
        a
    } else if b.is_nan() {
        b
    } else if a == 0.0 && b == 0.0 {
        if a.is_sign_positive() { a } else { b }
    } else if a >= b {
        a
    } else {
        b
    }
}

/// `PMath.floorI`: `(int) Math.floor(d)`; Rust's saturating cast matches Java's narrowing (NaN -> 0).
pub fn floor_i(value: f64) -> i32 {
    value.floor() as i32
}

/// `PMath.ceilI`: `(int) Math.ceil(d)`.
pub fn ceil_i(value: f64) -> i32 {
    value.ceil() as i32
}

/// `PMath.lerp`: `a + (b - a) * t`, evaluated without fused multiply-add.
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_max_follow_java_semantics() {
        assert!(java_min(f64::NAN, 1.0).is_nan());
        assert!(java_max(1.0, f64::NAN).is_nan());
        assert!(java_min(0.0, -0.0).is_sign_negative());
        assert!(java_max(-0.0, 0.0).is_sign_positive());
        assert_eq!(java_min(2.0, -3.0), -3.0);
    }

    #[test]
    fn integer_conversions_saturate_like_java() {
        assert_eq!(floor_i(-0.5), -1);
        assert_eq!(ceil_i(-0.5), 0);
        assert_eq!(floor_i(f64::NAN), 0);
        assert_eq!(floor_i(1e300), i32::MAX);
        assert_eq!(floor_i(-1e300), i32::MIN);
    }
}
