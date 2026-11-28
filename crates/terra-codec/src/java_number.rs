//! `java.lang.Double` parsing and ordering, shared by the ported command and OpenStreetMap parsers.

/// `Double.parseDouble`. `None` where Java throws `NumberFormatException`. Hexadecimal literals
/// with more than 13 significant digits are refused rather than rounded approximately.
pub fn parse_java_double(text: &str) -> Option<f64> {
    let trimmed = text.trim_matches(|c: char| c <= ' ');
    let (negative, body) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let signed = |value: f64| if negative { -value } else { value };
    match body {
        "NaN" => return Some(f64::NAN),
        "Infinity" => return Some(signed(f64::INFINITY)),
        _ => {}
    }
    let body = body.strip_suffix(['f', 'F', 'd', 'D']).unwrap_or(body);
    if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        return parse_hex_float(hex).map(signed);
    }
    let bytes = body.as_bytes();
    let mut index = 0;
    let digits = |index: &mut usize| {
        let start = *index;
        while bytes.get(*index).is_some_and(u8::is_ascii_digit) {
            *index += 1;
        }
        *index - start
    };
    let integer_digits = digits(&mut index);
    let mut fraction_digits = 0;
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        fraction_digits = digits(&mut index);
    }
    if integer_digits + fraction_digits == 0 {
        return None;
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        if digits(&mut index) == 0 {
            return None;
        }
    }
    if index != bytes.len() {
        return None;
    }
    let normalized = if body.starts_with('.') { format!("0{body}") } else { body.to_owned() };
    let normalized = normalized.replace(".e", ".0e").replace(".E", ".0E");
    normalized.parse::<f64>().ok().map(signed)
}

fn parse_hex_float(hex: &str) -> Option<f64> {
    let (mantissa_text, exponent_text) = hex.split_once(['p', 'P'])?;
    let (integer, fraction) = mantissa_text.split_once('.').unwrap_or((mantissa_text, ""));
    if integer.is_empty() && fraction.is_empty() {
        return None;
    }
    let digits: String = format!("{integer}{fraction}");
    let significant = digits.trim_start_matches('0');
    if significant.len() > 13 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let exponent_digits = exponent_text.strip_prefix(['+', '-']).unwrap_or(exponent_text);
    if exponent_digits.is_empty() || !exponent_digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let exponent: i32 = exponent_text.parse().unwrap_or(if exponent_text.starts_with('-') { -100_000 } else { 100_000 });
    let mantissa = if significant.is_empty() { 0 } else { u64::from_str_radix(significant, 16).ok()? };
    let scale = exponent.saturating_sub(4 * fraction.len() as i32);
    let value = mantissa as f64 * 2f64.powi(scale.clamp(-1100, 1100));
    (value.is_finite() || scale > 0).then_some(value)
}

/// `Double.compare(a, b)`: total order with -0.0 < 0.0 and every NaN equal and above everything.
pub fn java_double_compare(a: f64, b: f64) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => a.total_cmp(&b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn parses_java_double_grammar() {
        assert_eq!(parse_java_double(" 2 "), Some(2.0));
        assert_eq!(parse_java_double("1.5f"), Some(1.5));
        assert_eq!(parse_java_double("-.5e1"), Some(-5.0));
        assert_eq!(parse_java_double("0x1p3"), Some(8.0));
        assert_eq!(parse_java_double("-Infinity"), Some(f64::NEG_INFINITY));
        assert!(parse_java_double("NaN").is_some_and(f64::is_nan));
        for refused in ["", "2;3", "inf", "nan", "1e", ".", "0x", "1,5"] {
            assert_eq!(parse_java_double(refused), None, "{refused:?}");
        }
    }

    #[test]
    fn compares_like_double_compare() {
        assert_eq!(java_double_compare(-0.0, 0.0), Ordering::Less);
        assert_eq!(java_double_compare(f64::NAN, -f64::NAN), Ordering::Equal);
        assert_eq!(java_double_compare(f64::NAN, f64::INFINITY), Ordering::Greater);
    }
}
