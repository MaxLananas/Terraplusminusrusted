//! `java.lang.Math` equivalents used by the ported algorithms.
//!
//! `toRadians`/`toDegrees` use the exact constants of OpenJDK 9+ (`DEGREES_TO_RADIANS`,
//! `RADIANS_TO_DEGREES`). The transcendental functions delegate to the platform libm; Java only
//! guarantees 1 ulp accuracy for `Math` (its results already vary between JVM intrinsics and
//! `StrictMath`), so bit-for-bit agreement is not a property upstream has either.

const DEGREES_TO_RADIANS: f64 = 0.017_453_292_519_943_295;
const RADIANS_TO_DEGREES: f64 = 57.295_779_513_082_32;

#[inline]
pub(crate) fn to_radians(degrees: f64) -> f64 {
    degrees * DEGREES_TO_RADIANS
}

#[inline]
pub(crate) fn to_degrees(radians: f64) -> f64 {
    radians * RADIANS_TO_DEGREES
}

#[inline]
pub(crate) fn sin(value: f64) -> f64 {
    value.sin()
}

#[inline]
pub(crate) fn cos(value: f64) -> f64 {
    value.cos()
}

#[inline]
pub(crate) fn tan(value: f64) -> f64 {
    value.tan()
}

#[inline]
pub(crate) fn atan(value: f64) -> f64 {
    value.atan()
}

#[inline]
pub(crate) fn asin(value: f64) -> f64 {
    value.asin()
}

#[inline]
pub(crate) fn acos(value: f64) -> f64 {
    value.acos()
}

#[inline]
pub(crate) fn sinh(value: f64) -> f64 {
    value.sinh()
}

#[inline]
pub(crate) fn cosh(value: f64) -> f64 {
    value.cosh()
}

#[inline]
pub(crate) fn atan2(y: f64, x: f64) -> f64 {
    y.atan2(x)
}

#[inline]
pub(crate) fn exp(value: f64) -> f64 {
    value.exp()
}

#[inline]
pub(crate) fn ln(value: f64) -> f64 {
    value.ln()
}

#[inline]
pub(crate) fn pow(base: f64, exponent: f64) -> f64 {
    base.powf(exponent)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angle_conversions_match_openjdk_constants() {
        assert_eq!(to_radians(180.0), std::f64::consts::PI);
        assert_eq!(to_degrees(std::f64::consts::PI), 180.0);
        assert_eq!(DEGREES_TO_RADIANS, std::f64::consts::PI / 180.0);
        assert_eq!(RADIANS_TO_DEGREES, 180.0 / std::f64::consts::PI);
    }
}
