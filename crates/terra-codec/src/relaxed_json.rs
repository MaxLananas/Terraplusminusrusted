//! The JSON dialect TerraMinusMinus reads its `.json5` configs with (`TerraConstants.JSON_MAPPER`):
//! strict JSON plus Java comments, leading zeros, leading decimal points, `NaN`/`Infinity` and
//! trailing commas. Unquoted keys and single-quoted strings are rejected, as Jackson rejects them.

use std::fmt;

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    /// Members in document order; duplicate keys are rejected at parse time.
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            Self::Object(members) => members.iter().find(|(name, _)| name == key).map(|(_, value)| value),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(String, JsonValue)]> {
        match self {
            Self::Object(members) => Some(members),
            _ => None,
        }
    }

    /// `ACCEPT_SINGLE_VALUE_AS_ARRAY`: an array's elements, or the value itself as one element.
    pub fn as_array_lenient(&self) -> &[JsonValue] {
        match self {
            Self::Array(items) => items,
            other => std::slice::from_ref(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}, column {}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for JsonError {}

pub fn parse_relaxed_json(text: &str) -> Result<JsonValue, JsonError> {
    let mut parser = Parser { bytes: text.as_bytes(), position: 0 };
    let value = parser.value(0)?;
    parser.skip_insignificant()?;
    if parser.position != parser.bytes.len() {
        return Err(parser.error("trailing content after the document"));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Parser<'_> {
    fn error(&self, message: impl Into<String>) -> JsonError {
        let consumed = &self.bytes[..self.position.min(self.bytes.len())];
        let line = consumed.iter().filter(|&&b| b == b'\n').count() + 1;
        let column = consumed.iter().rev().take_while(|&&b| b != b'\n').count() + 1;
        JsonError { line, column, message: message.into() }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_insignificant(&mut self) -> Result<(), JsonError> {
        loop {
            match (self.peek(), self.bytes.get(self.position + 1)) {
                (Some(b' ' | b'\t' | b'\n' | b'\r'), _) => self.position += 1,
                (Some(b'/'), Some(b'/')) => {
                    while self.peek().is_some_and(|b| b != b'\n') {
                        self.position += 1;
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    let start = self.position;
                    self.position += 2;
                    loop {
                        match self.peek() {
                            None => {
                                self.position = start;
                                return Err(self.error("unterminated block comment"));
                            }
                            Some(b'*') if self.bytes.get(self.position + 1) == Some(&b'/') => {
                                self.position += 2;
                                break;
                            }
                            Some(_) => self.position += 1,
                        }
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn expect_literal(&mut self, literal: &str) -> Result<(), JsonError> {
        if self.bytes[self.position..].starts_with(literal.as_bytes()) {
            self.position += literal.len();
            Ok(())
        } else {
            Err(self.error(format!("expected `{literal}`")))
        }
    }

    fn value(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        if depth > MAX_DEPTH {
            return Err(self.error("nesting exceeds 64 levels"));
        }
        self.skip_insignificant()?;
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(JsonValue::String),
            Some(b't') => self.expect_literal("true").map(|()| JsonValue::Bool(true)),
            Some(b'f') => self.expect_literal("false").map(|()| JsonValue::Bool(false)),
            Some(b'n') => self.expect_literal("null").map(|()| JsonValue::Null),
            Some(b'-' | b'+' | b'.' | b'0'..=b'9' | b'N' | b'I') => self.number().map(JsonValue::Number),
            Some(other) => Err(self.error(format!("unexpected character `{}`", char::from(other)))),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.position += 1;
        let mut members: Vec<(String, JsonValue)> = Vec::new();
        loop {
            self.skip_insignificant()?;
            match self.peek() {
                Some(b'}') => {
                    self.position += 1;
                    return Ok(JsonValue::Object(members));
                }
                Some(b'"') => {}
                _ => return Err(self.error("expected a double-quoted member name or `}`")),
            }
            let key = self.string()?;
            if members.iter().any(|(name, _)| *name == key) {
                return Err(self.error(format!("duplicate member `{key}`")));
            }
            self.skip_insignificant()?;
            if self.peek() != Some(b':') {
                return Err(self.error("expected `:`"));
            }
            self.position += 1;
            let value = self.value(depth + 1)?;
            members.push((key, value));
            self.skip_insignificant()?;
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {}
                _ => return Err(self.error("expected `,` or `}`")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<JsonValue, JsonError> {
        self.position += 1;
        let mut items = Vec::new();
        loop {
            self.skip_insignificant()?;
            if self.peek() == Some(b']') {
                self.position += 1;
                return Ok(JsonValue::Array(items));
            }
            items.push(self.value(depth + 1)?);
            self.skip_insignificant()?;
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {}
                _ => return Err(self.error("expected `,` or `]`")),
            }
        }
    }

    fn string(&mut self) -> Result<String, JsonError> {
        self.position += 1;
        let mut out = String::new();
        loop {
            let start = self.position;
            while self.peek().is_some_and(|b| b != b'"' && b != b'\\' && b >= 0x20) {
                self.position += 1;
            }
            let chunk = std::str::from_utf8(&self.bytes[start..self.position]).map_err(|_| self.error("invalid UTF-8"))?;
            out.push_str(chunk);
            match self.peek() {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.position += 1;
                    let escaped = self.peek().ok_or_else(|| self.error("unterminated escape"))?;
                    self.position += 1;
                    match escaped {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode_escape()?),
                        _ => return Err(self.error("invalid escape")),
                    }
                }
                Some(_) => return Err(self.error("control character in string")),
                None => return Err(self.error("unterminated string")),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, JsonError> {
        let digits = self.bytes.get(self.position..self.position + 4).ok_or_else(|| self.error("truncated \\u escape"))?;
        let text = std::str::from_utf8(digits).map_err(|_| self.error("invalid \\u escape"))?;
        let value = u32::from_str_radix(text, 16).map_err(|_| self.error("invalid \\u escape"))?;
        self.position += 4;
        Ok(value)
    }

    fn unicode_escape(&mut self) -> Result<char, JsonError> {
        let high = self.hex4()?;
        let code = if (0xD800..0xDC00).contains(&high) {
            self.expect_literal("\\u")?;
            let low = self.hex4()?;
            if !(0xDC00..0xE000).contains(&low) {
                return Err(self.error("unpaired surrogate"));
            }
            0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)
        } else {
            high
        };
        char::from_u32(code).ok_or_else(|| self.error("invalid code point"))
    }

    fn number(&mut self) -> Result<f64, JsonError> {
        let start = self.position;
        let negative = match self.peek() {
            Some(b'-') => {
                self.position += 1;
                true
            }
            Some(b'+') => {
                self.position += 1;
                false
            }
            _ => false,
        };
        if self.peek() == Some(b'N') {
            if start != self.position {
                return Err(self.error("signed NaN"));
            }
            self.expect_literal("NaN")?;
            return Ok(f64::NAN);
        }
        if self.peek() == Some(b'I') {
            self.expect_literal("Infinity")?;
            return Ok(if negative { f64::NEG_INFINITY } else { f64::INFINITY });
        }
        let digits_start = self.position;
        while self.peek().is_some_and(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')) {
            self.position += 1;
        }
        let text = std::str::from_utf8(&self.bytes[digits_start..self.position]).map_err(|_| self.error("invalid number"))?;
        let normalized = if text.starts_with('.') { format!("0{text}") } else { text.to_owned() };
        if normalized.is_empty() || normalized.ends_with('.') || !normalized.as_bytes()[0].is_ascii_digit() {
            self.position = start;
            return Err(self.error(format!("invalid number `{text}`")));
        }
        let magnitude: f64 = normalized.parse().map_err(|_| {
            self.position = start;
            self.error(format!("invalid number `{text}`"))
        })?;
        Ok(if negative { -magnitude } else { magnitude })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_jackson_relaxations() {
        let value = parse_relaxed_json("/* c */ [ // x\n {\"a\": .5, \"b\": 007, \"c\": -Infinity, \"d\": NaN, \"e\": [1,],},\n]").unwrap();
        let object = &value.as_array_lenient()[0];
        assert_eq!(object.get("a").and_then(JsonValue::as_f64), Some(0.5));
        assert_eq!(object.get("b").and_then(JsonValue::as_f64), Some(7.0));
        assert_eq!(object.get("c").and_then(JsonValue::as_f64), Some(f64::NEG_INFINITY));
        assert!(object.get("d").and_then(JsonValue::as_f64).is_some_and(f64::is_nan));
    }

    #[test]
    fn decodes_escapes_and_surrogates() {
        let value = parse_relaxed_json(r#""a\"b\u00e9\ud83d\ude00\/""#).unwrap();
        assert_eq!(value.as_str(), Some("a\"b\u{e9}\u{1F600}/"));
    }

    #[test]
    fn rejects_what_jackson_rejects() {
        for input in ["{a: 1}", "{'a': 1}", "[1 2]", "{\"a\": 1, \"a\": 2}", "1.", "[", "/* open", "\"\u{1}\"", "-NaN", "1 2"] {
            assert!(parse_relaxed_json(input).is_err(), "{input:?} accepted");
        }
        let error = parse_relaxed_json("{\n  \"a\": x}").unwrap_err();
        assert_eq!((error.line, error.column), (2, 8));
    }

    #[test]
    fn bounds_nesting_depth() {
        let deep = "[".repeat(100) + &"]".repeat(100);
        assert!(parse_relaxed_json(&deep).is_err());
    }
}
