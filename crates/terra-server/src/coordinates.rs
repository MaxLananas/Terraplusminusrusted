//! `CoordinateParseUtils.parseVerbatimCoordinates` (TerraMinusMinus), the parser behind `/tpll`.

use crate::java_text::{Captures, JavaPattern, java_double_compare, java_round, parse_java_double};
use std::cmp::Ordering;
use std::sync::OnceLock;

const DMS: &str = concat!(
    r"\s*(\d{1,3})\s*(?:°|d|º| |g|o)",
    r"\s*([0-6]?\d)\s*(?:'|m| |´|’|′)",
    r"\s*(?:",
    r"([0-6]?\d(?:[,.]\d+)?)",
    r#"\s*(?:"|''|s|´´|″)?"#,
    r")?\s*"
);
const DM: &str = concat!(r"\s*(\d{1,3})\s*(?:°|d|º| |g|o)", r"\s*(?:", r"([0-6]?\d(?:[,.]\d+)?)", r"\s*(?:'|m| |´|’|′)?", r")?\s*");
const D: &str = r"\s*(\d{1,3}(?:[,.]\d+)?)\s*(?:°|d|º| |g|o|)\s*";
const POSITIVE: &str = "NEO";
const PART_DELIMITERS: [char; 3] = [';', '/', ','];

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LatLng {
    pub latitude: f64,
    pub longitude: f64,
}

/// How upstream ends for an input, including the inputs where it throws instead of answering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UpstreamParseOutcome {
    Parsed(LatLng),
    Unparseable,
    /// Upstream throws `NullPointerException` (an optional regex group was absent), which
    /// reaches the command as an internal error.
    NullGroup,
}

struct Patterns {
    dms_single: JavaPattern,
    dm_single: JavaPattern,
    d_single: JavaPattern,
    dms_coordinates: JavaPattern,
}

fn patterns() -> Option<&'static Patterns> {
    static PATTERNS: OnceLock<Option<Patterns>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            Some(Patterns {
                dms_single: JavaPattern::compile(&format!("^{DMS}$")).ok()?,
                dm_single: JavaPattern::compile(&format!("^{DM}$")).ok()?,
                d_single: JavaPattern::compile(&format!("^{D}$")).ok()?,
                dms_coordinates: JavaPattern::compile(&format!("^{DMS}([NSEOW])[ ,;/]?{DMS}([NSEOW])$")).ok()?,
            })
        })
        .as_ref()
}

#[derive(Debug)]
enum Failure {
    /// `NumberFormatException`, `IllegalArgumentException` or `OutOfProjectionBoundsException`.
    Rejected,
    NullGroup,
}

type NumberFormat = fn(&str) -> Option<f64>;

fn comma_decimal(text: &str) -> Option<f64> {
    if text.find('.').is_some_and(|index| index > 0) {
        return None;
    }
    parse_java_double(&text.replace(',', "."))
}

const NUMBER_FORMATS: [NumberFormat; 2] = [parse_java_double, comma_decimal];

fn round_to_6_decimals(value: f64) -> f64 {
    java_round(value * 1e6) as f64 / 1e6
}

fn in_range(lat: f64, lon: f64) -> bool {
    java_double_compare(lat, 90.0) != Ordering::Greater
        && java_double_compare(lat, -90.0) != Ordering::Less
        && java_double_compare(lon, 180.0) != Ordering::Greater
        && java_double_compare(lon, -180.0) != Ordering::Less
}

fn coordinate_sign(direction: &str) -> f64 {
    if POSITIVE.contains(direction.to_uppercase().as_str()) { 1.0 } else { -1.0 }
}

fn is_latitude(direction: &str) -> bool {
    "NS".contains(direction.to_uppercase().as_str())
}

fn group<'a>(input: &[char], captures: &Captures, index: usize, scratch: &'a mut String) -> Option<&'a str> {
    let (start, end) = captures.get(index).copied().flatten()?;
    *scratch = input[start..end].iter().collect();
    Some(scratch.as_str())
}

fn apply(format: NumberFormat, text: Option<&str>) -> Result<f64, Failure> {
    format(text.ok_or(Failure::NullGroup)?).ok_or(Failure::Rejected)
}

fn coordinate_from_groups(input: &[char], captures: &Captures, indices: &[usize], sign: &str, format: NumberFormat) -> Result<f64, Failure> {
    let mut parts = [0.0; 3];
    for (slot, &index) in parts.iter_mut().zip(indices) {
        let mut scratch = String::new();
        *slot = apply(format, group(input, captures, index, &mut scratch))?;
    }
    Ok(round_to_6_decimals(coordinate_sign(sign) * (parts[0] + parts[1] / 60.0 + parts[2] / 3600.0)))
}

fn validate_and_round(lat: f64, lon: f64) -> Result<LatLng, Failure> {
    let (lat, lon) = (round_to_6_decimals(lat), round_to_6_decimals(lon));
    if java_double_compare(lat, 0.0) == Ordering::Equal && java_double_compare(lon, 0.0) == Ordering::Equal {
        return Ok(LatLng { latitude: 0.0, longitude: 0.0 });
    }
    if in_range(lat, lon) {
        return Ok(LatLng { latitude: lat, longitude: lon });
    }
    if (java_double_compare(lat, 90.0) == Ordering::Greater || java_double_compare(lat, -90.0) == Ordering::Less) && in_range(lon, lat) {
        return Ok(LatLng { latitude: lat, longitude: lon });
    }
    Err(Failure::Rejected)
}

fn separate_parts(coordinates: &str) -> Option<(String, String)> {
    let chars: Vec<char> = coordinates.trim_matches(|c: char| c <= ' ').chars().collect();
    if let Some(mut first_blank_start) = chars.iter().position(|&c| c == ' ').filter(|&index| index > 0) {
        let mut first_blank_end = first_blank_start;
        while chars[first_blank_end] == ' ' {
            first_blank_end += 1;
        }
        if chars[first_blank_end..].contains(&' ') {
            return None;
        }
        let before = chars[first_blank_start - 1];
        if first_blank_start > 1 && PART_DELIMITERS.contains(&before) && !chars[first_blank_end..].contains(&before) {
            first_blank_start -= 1;
        }
        return Some((chars[..first_blank_start].iter().collect(), chars[first_blank_end..].iter().collect()));
    }
    for delimiter in PART_DELIMITERS {
        let count = chars.iter().filter(|&&c| c == delimiter).count();
        if let Some(index) = chars.iter().position(|&c| c == delimiter)
            && count == 1
            && index > 0
            && index < chars.len() - 1
        {
            return Some((chars[..index].iter().collect(), chars[index + 1..].iter().collect()));
        }
    }
    None
}

fn parse_dms(coordinate: &str, latitude: bool, format: NumberFormat, patterns: &Patterns) -> Result<f64, Failure> {
    let directions = if latitude { "NS" } else { "EOW" };
    let mut chars: Vec<char> = coordinate.trim_matches(|c: char| c <= ' ').to_uppercase().chars().collect();
    if chars.len() <= 3 {
        return Err(Failure::Rejected);
    }
    let mut direction = 'n';
    if let Some(&first) = chars.first().filter(|c| directions.contains(**c)) {
        direction = first;
        chars.remove(0);
    } else if let Some(&last) = chars.last().filter(|c| directions.contains(**c)) {
        direction = last;
        chars.pop();
    }
    let sign = direction.to_string();
    if let Some(captures) = patterns.dms_single.find(&chars) {
        return coordinate_from_groups(&chars, &captures, &[1, 2, 3], &sign, format);
    }
    if let Some(captures) = patterns.dm_single.find(&chars) {
        return coordinate_from_groups(&chars, &captures, &[1, 2], &sign, format);
    }
    if let Some(captures) = patterns.d_single.find(&chars) {
        return coordinate_from_groups(&chars, &captures, &[1], &sign, format);
    }
    Err(Failure::Rejected)
}

fn parse_decimal(coordinate: &str, format: NumberFormat) -> Option<f64> {
    match coordinate.strip_suffix('°') {
        Some(stripped) => format(stripped.trim_matches(|c: char| c <= ' ')),
        None => format(coordinate),
    }
}

/// The exact upstream outcome, including where upstream throws.
pub fn parse_coordinates_upstream(coordinates: &str) -> UpstreamParseOutcome {
    let Some(patterns) = patterns() else { return UpstreamParseOutcome::Unparseable };
    if coordinates.is_empty() {
        return UpstreamParseOutcome::Unparseable;
    }
    let input: Vec<char> = coordinates.chars().collect();
    if let Some(captures) = patterns.dms_coordinates.find(&input) {
        let mut first = String::new();
        let mut second = String::new();
        let direction1 = group(&input, &captures, 4, &mut first).unwrap_or("").to_owned();
        let direction2 = group(&input, &captures, 8, &mut second).unwrap_or("").to_owned();
        for format in NUMBER_FORMATS {
            let attempt = (|| {
                let coordinate1 = coordinate_from_groups(&input, &captures, &[1, 2, 3], &direction1, format)?;
                let coordinate2 = coordinate_from_groups(&input, &captures, &[5, 6, 7], &direction2, format)?;
                if is_latitude(&direction1) && !is_latitude(&direction2) {
                    if coordinate2.abs() > 180.0 || coordinate1.abs() > 90.0 {
                        return Err(Failure::Rejected);
                    }
                    validate_and_round(coordinate1, coordinate2).map(Some)
                } else if !is_latitude(&direction1) && is_latitude(&direction2) {
                    validate_and_round(coordinate2, coordinate1).map(Some)
                } else {
                    Ok(None)
                }
            })();
            match attempt {
                Ok(Some(parsed)) => return UpstreamParseOutcome::Parsed(parsed),
                Ok(None) => return UpstreamParseOutcome::Unparseable,
                Err(_) => {}
            }
        }
        return UpstreamParseOutcome::Unparseable;
    }
    let Some((first, second)) = separate_parts(coordinates) else { return UpstreamParseOutcome::Unparseable };
    for format in NUMBER_FORMATS {
        let attempt = parse_dms(&first, true, format, patterns).and_then(|lat| parse_dms(&second, false, format, patterns).map(|lon| (lat, lon)));
        match attempt.and_then(|(lat, lon)| validate_and_round(lat, lon)) {
            Ok(parsed) => return UpstreamParseOutcome::Parsed(parsed),
            Err(Failure::NullGroup) => return UpstreamParseOutcome::NullGroup,
            Err(Failure::Rejected) => {}
        }
    }
    for format in NUMBER_FORMATS {
        if let (Some(lat), Some(lon)) = (parse_decimal(&first, format), parse_decimal(&second, format))
            && let Ok(parsed) = validate_and_round(lat, lon)
        {
            return UpstreamParseOutcome::Parsed(parsed);
        }
    }
    UpstreamParseOutcome::Unparseable
}

/// `parseVerbatimCoordinates`; inputs upstream fails on with an internal error are unparseable.
pub fn parse_coordinates(coordinates: &str) -> Option<LatLng> {
    match parse_coordinates_upstream(coordinates) {
        UpstreamParseOutcome::Parsed(parsed) => Some(parsed),
        UpstreamParseOutcome::Unparseable | UpstreamParseOutcome::NullGroup => None,
    }
}
