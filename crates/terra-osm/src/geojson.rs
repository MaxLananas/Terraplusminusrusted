//! The GeoJSON subset TerraMinusMinus' `ObjectDeserializer` reads: one object per line, `type`
//! first, only the members it knows, plus the non-standard `Reference` object. Reading follows its
//! Gson streaming calls, including what they reject (missing geometry, unknown members, rings that
//! are not closed) so that a tile upstream refuses is refused here too.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use terra_codec::{java_double_compare, parse_java_double};

const MAX_DEPTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    pub lon: f64,
    pub lat: f64,
}

impl GeoPoint {
    /// Lombok `@Data` equality: each coordinate by `Double.compare`.
    fn same_as(&self, other: &GeoPoint) -> bool {
        java_double_compare(self.lon, other.lon).is_eq() && java_double_compare(self.lat, other.lat).is_eq()
    }
}

/// `LineString.isLinearRing`.
pub fn is_linear_ring<P>(points: &[P], same: impl Fn(&P, &P) -> bool) -> bool {
    points.len() >= 4 && points.first().zip(points.last()).is_some_and(|(first, last)| same(first, last))
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeoPolygon {
    pub outer: Vec<GeoPoint>,
    pub inner: Vec<Vec<GeoPoint>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Geometry {
    Point(GeoPoint),
    MultiPoint(Vec<GeoPoint>),
    LineString(Vec<GeoPoint>),
    MultiLineString(Vec<Vec<GeoPoint>>),
    Polygon(GeoPolygon),
    MultiPolygon(Vec<GeoPolygon>),
    GeometryCollection(Vec<Geometry>),
}

pub type Tags = HashMap<String, String>;

#[derive(Debug, Clone, PartialEq)]
pub struct Feature {
    pub geometry: Geometry,
    pub properties: Option<Arc<Tags>>,
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GeoJsonObject {
    Geometry(Geometry),
    Feature(Feature),
    FeatureCollection(Vec<Feature>),
    /// Another resource, by key relative to the dataset's servers, whose objects belong here.
    Reference(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeoJsonError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for GeoJsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GeoJSON line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for GeoJsonError {}

/// `ParsingGeoJsonDataset.decode`: each line (split like `BufferedReader.lines`) is one object.
/// Bytes are decoded as UTF-8 with replacement, as Java's default charset decoder does.
pub fn parse_geojson_lines(bytes: &[u8]) -> Result<Vec<GeoJsonObject>, GeoJsonError> {
    let text = String::from_utf8_lossy(bytes);
    let mut objects = Vec::new();
    for (index, line) in java_lines(&text).enumerate() {
        objects.push(parse_geojson(line).map_err(|message| GeoJsonError { line: index + 1, message })?);
    }
    Ok(objects)
}

fn java_lines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = rest.find(['\n', '\r']).unwrap_or(rest.len());
        let line = &rest[..end];
        let mut next = end;
        if rest[end..].starts_with("\r\n") {
            next += 2;
        } else if end < rest.len() {
            next += 1;
        }
        rest = &rest[next..];
        Some(line)
    })
}

/// `GeoJson.parse(String)`. A blank document yields no object upstream, which then fails later;
/// it is an error here.
pub fn parse_geojson(text: &str) -> Result<GeoJsonObject, String> {
    let mut reader = Reader { bytes: text.as_bytes(), position: 0 };
    reader.skip_insignificant()?;
    if reader.position == reader.bytes.len() {
        return Err("blank line where a GeoJSON object was expected".into());
    }
    let object = reader.object()?;
    reader.skip_insignificant()?;
    if reader.position != reader.bytes.len() {
        return Err("JSON document was not fully consumed".into());
    }
    Ok(object)
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_insignificant(&mut self) -> Result<(), String> {
        loop {
            match (self.peek(), self.bytes.get(self.position + 1)) {
                (Some(b' ' | b'\t' | b'\n' | b'\r'), _) => self.position += 1,
                (Some(b'#'), _) | (Some(b'/'), Some(b'/')) => {
                    while self.peek().is_some_and(|b| b != b'\n' && b != b'\r') {
                        self.position += 1;
                    }
                }
                (Some(b'/'), Some(b'*')) => {
                    let end = self.bytes[self.position + 2..].windows(2).position(|pair| pair == b"*/").ok_or("unterminated comment")?;
                    self.position += end + 4;
                }
                _ => return Ok(()),
            }
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        self.skip_insignificant()?;
        if self.peek() == Some(byte) {
            self.position += 1;
            Ok(())
        } else {
            Err(format!("expected `{}` at byte {}", char::from(byte), self.position))
        }
    }

    /// The next member name of the current object, or `None` after consuming its `}`.
    fn next_name(&mut self, first: &mut bool) -> Result<Option<String>, String> {
        self.skip_insignificant()?;
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(None);
        }
        if !*first {
            self.expect(b',')?;
            self.skip_insignificant()?;
        }
        *first = false;
        let name = match self.peek() {
            Some(quote @ (b'"' | b'\'')) => self.quoted(quote)?,
            _ => return Err(format!("expected a member name at byte {}", self.position)),
        };
        self.expect(b':')?;
        Ok(Some(name))
    }

    fn required_name(&mut self, first: &mut bool, expected: &str, context: &str) -> Result<(), String> {
        match self.next_name(first)? {
            Some(name) if name == expected => Ok(()),
            Some(name) => Err(format!("unexpected field \"{name}\" in {context}")),
            None => Err(format!("missing \"{expected}\" in {context}")),
        }
    }

    fn end_object(&mut self, first: &mut bool, context: &str) -> Result<(), String> {
        match self.next_name(first)? {
            None => Ok(()),
            Some(name) => Err(format!("unexpected field \"{name}\" in {context}")),
        }
    }

    /// Whether the current array has another element; consumes its `]` when it does not.
    fn has_next(&mut self, first: &mut bool) -> Result<bool, String> {
        self.skip_insignificant()?;
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(false);
        }
        if !*first {
            self.expect(b',')?;
        }
        *first = false;
        Ok(true)
    }

    fn quoted(&mut self, quote: u8) -> Result<String, String> {
        self.position += 1;
        let mut out = String::new();
        loop {
            let start = self.position;
            while self.peek().is_some_and(|b| b != quote && b != b'\\') {
                self.position += 1;
            }
            out.push_str(std::str::from_utf8(&self.bytes[start..self.position]).map_err(|_| "invalid UTF-8 in string")?);
            match self.peek() {
                Some(b'\\') => {
                    self.position += 1;
                    let escaped = self.peek().ok_or("unterminated escape")?;
                    self.position += 1;
                    match escaped {
                        b'u' => out.push(self.unicode_escape()?),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'"' | b'\'' | b'\\' | b'/' => out.push(char::from(escaped)),
                        b'\n' => out.push('\n'),
                        _ => return Err("invalid escape sequence".into()),
                    }
                }
                Some(_) => {
                    self.position += 1;
                    return Ok(out);
                }
                None => return Err("unterminated string".into()),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.bytes.get(self.position..self.position + 4).ok_or("truncated \\u escape")?;
        let text = std::str::from_utf8(digits).map_err(|_| "invalid \\u escape")?;
        let value = u32::from_str_radix(text, 16).map_err(|_| "invalid \\u escape")?;
        self.position += 4;
        Ok(value)
    }

    /// Java strings hold UTF-16 code units; an unpaired surrogate becomes U+FFFD here.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let high = self.hex4()?;
        if (0xD800..0xDC00).contains(&high) && self.bytes[self.position..].starts_with(b"\\u") {
            let saved = self.position;
            self.position += 2;
            let low = self.hex4()?;
            if (0xDC00..0xE000).contains(&low) {
                return Ok(char::from_u32(0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00)).unwrap_or('\u{FFFD}'));
            }
            self.position = saved;
        }
        Ok(char::from_u32(high).unwrap_or('\u{FFFD}'))
    }

    /// The literal text of a number or of the lenient `NaN`/`Infinity` literals.
    fn literal(&mut self) -> Option<String> {
        let start = self.position;
        for word in ["NaN", "-Infinity", "Infinity"] {
            if self.bytes[start..].starts_with(word.as_bytes()) {
                self.position += word.len();
                return Some(word.to_owned());
            }
        }
        while self.peek().is_some_and(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E')) {
            self.position += 1;
        }
        (self.position > start).then(|| String::from_utf8_lossy(&self.bytes[start..self.position]).into_owned())
    }

    /// Whether the next value is a JSON number (Gson's `JsonToken.NUMBER`).
    fn at_number(&mut self) -> Result<bool, String> {
        self.skip_insignificant()?;
        Ok(match self.peek() {
            Some(b'0'..=b'9') => true,
            Some(b'-') => self.bytes.get(self.position + 1).is_some_and(u8::is_ascii_digit),
            _ => false,
        })
    }

    /// Gson `nextString`: a string, or a number's literal text.
    fn next_string(&mut self) -> Result<String, String> {
        self.skip_insignificant()?;
        match self.peek() {
            Some(quote @ (b'"' | b'\'')) => self.quoted(quote),
            _ => self.literal().ok_or_else(|| format!("expected a string at byte {}", self.position)),
        }
    }

    /// Gson lenient `nextDouble`: a number, or a string holding one.
    fn next_double(&mut self) -> Result<f64, String> {
        let text = self.next_string()?;
        parse_java_double(&text).ok_or_else(|| format!("expected a number, found `{text}`"))
    }

    fn object(&mut self) -> Result<GeoJsonObject, String> {
        self.expect(b'{')?;
        let mut first = true;
        self.required_name(&mut first, "type", "GeoJSON object (it must start with type)")?;
        let kind = self.next_string()?;
        let object = match kind.as_str() {
            "Feature" => GeoJsonObject::Feature(self.feature_members(&mut first)?),
            "FeatureCollection" => {
                self.required_name(&mut first, "features", "FeatureCollection object")?;
                let features = self.features()?;
                self.end_object(&mut first, "FeatureCollection object")?;
                GeoJsonObject::FeatureCollection(features)
            }
            "Reference" => {
                self.required_name(&mut first, "location", "Reference object")?;
                let location = self.next_string()?;
                self.end_object(&mut first, "Reference object")?;
                GeoJsonObject::Reference(location)
            }
            other => {
                let geometry = self.geometry_body(other, &mut first, 0)?;
                self.end_object(&mut first, "geometry object")?;
                GeoJsonObject::Geometry(geometry)
            }
        };
        Ok(object)
    }

    fn features(&mut self) -> Result<Vec<Feature>, String> {
        self.expect(b'[')?;
        let mut features = Vec::new();
        let mut first_element = true;
        while self.has_next(&mut first_element)? {
            self.expect(b'{')?;
            let mut first = true;
            self.required_name(&mut first, "type", "FeatureCollection member (it must start with type)")?;
            let kind = self.next_string()?;
            if kind != "Feature" {
                return Err(format!("FeatureCollection contains non-Feature element \"{kind}\""));
            }
            features.push(self.feature_members(&mut first)?);
        }
        Ok(features)
    }

    fn feature_members(&mut self, first: &mut bool) -> Result<Feature, String> {
        let mut geometry = None;
        let mut properties = None;
        let mut id = None;
        while let Some(name) = self.next_name(first)? {
            match name.as_str() {
                "geometry" => geometry = Some(self.geometry(0)?),
                "properties" => {
                    self.skip_insignificant()?;
                    if self.bytes[self.position..].starts_with(b"null") {
                        self.position += 4;
                        properties = None;
                    } else {
                        properties = Some(Arc::new(self.properties()?));
                    }
                }
                "id" => id = Some(self.next_string()?),
                other => return Err(format!("invalid field name \"{other}\" in Feature")),
            }
        }
        let geometry = geometry.ok_or("Feature without geometry")?;
        Ok(Feature { geometry, properties, id })
    }

    fn properties(&mut self) -> Result<Tags, String> {
        self.expect(b'{')?;
        let mut tags = Tags::new();
        let mut first = true;
        while let Some(name) = self.next_name(&mut first)? {
            let value = self.next_string()?;
            tags.entry(name).or_insert(value);
        }
        Ok(tags)
    }

    fn geometry(&mut self, depth: usize) -> Result<Geometry, String> {
        if depth > MAX_DEPTH {
            return Err("geometry nesting exceeds 32 levels".into());
        }
        self.expect(b'{')?;
        let mut first = true;
        self.required_name(&mut first, "type", "geometry (it must start with type)")?;
        let kind = self.next_string()?;
        let geometry = self.geometry_body(&kind, &mut first, depth)?;
        self.end_object(&mut first, "geometry object")?;
        Ok(geometry)
    }

    fn geometry_body(&mut self, kind: &str, first: &mut bool, depth: usize) -> Result<Geometry, String> {
        let field = self.next_name(first)?.ok_or_else(|| format!("{kind} object without coordinates"))?;
        if kind == "GeometryCollection" {
            if field != "geometries" {
                return Err(format!("unexpected field \"{field}\" in GeometryCollection object"));
            }
            self.expect(b'[')?;
            let mut geometries = Vec::new();
            let mut first_element = true;
            while self.has_next(&mut first_element)? {
                geometries.push(self.geometry(depth + 1)?);
            }
            return Ok(Geometry::GeometryCollection(geometries));
        }
        if field != "coordinates" {
            return Err(format!("unexpected field \"{field}\" in {kind} object"));
        }
        Ok(match kind {
            "Point" => Geometry::Point(self.point()?),
            "MultiPoint" => Geometry::MultiPoint(self.points()?),
            "LineString" => Geometry::LineString(self.line_string()?),
            "MultiLineString" => Geometry::MultiLineString(self.list(Self::line_string)?),
            "Polygon" => Geometry::Polygon(self.polygon()?),
            "MultiPolygon" => Geometry::MultiPolygon(self.list(Self::polygon)?),
            other => return Err(format!("unknown GeoJSON type \"{other}\"")),
        })
    }

    fn point(&mut self) -> Result<GeoPoint, String> {
        self.expect(b'[')?;
        let lon = self.next_double()?;
        self.expect(b',')?;
        let lat = self.next_double()?;
        self.skip_insignificant()?;
        if self.peek() == Some(b',') {
            self.position += 1;
            if !self.at_number()? {
                return Err("a point's optional third coordinate must be a number".into());
            }
            self.next_double()?;
        }
        self.expect(b']')?;
        Ok(GeoPoint { lon, lat })
    }

    fn list<T>(&mut self, mut element: impl FnMut(&mut Self) -> Result<T, String>) -> Result<Vec<T>, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        let mut first = true;
        while self.has_next(&mut first)? {
            items.push(element(self)?);
        }
        Ok(items)
    }

    fn points(&mut self) -> Result<Vec<GeoPoint>, String> {
        self.list(Self::point)
    }

    fn line_string(&mut self) -> Result<Vec<GeoPoint>, String> {
        let points = self.points()?;
        if points.len() < 2 {
            return Err("LineString must contain at least 2 points".into());
        }
        Ok(points)
    }

    fn polygon(&mut self) -> Result<GeoPolygon, String> {
        let mut rings = self.list(Self::line_string)?.into_iter();
        let outer = rings.next().ok_or("Polygon without rings")?;
        let inner: Vec<Vec<GeoPoint>> = rings.collect();
        if !is_linear_ring(&outer, GeoPoint::same_as) {
            return Err("outerRing is not a linear ring".into());
        }
        if let Some(index) = inner.iter().position(|ring| !is_linear_ring(ring, GeoPoint::same_as)) {
            return Err(format!("innerRings[{index}] is not a linear ring"));
        }
        Ok(GeoPolygon { outer, inner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_features_collections_and_references() {
        let text = concat!(
            r#"{"type":"Feature","id":"way/1","properties":{"highway":"primary","lanes":3,"highway":"x"},"geometry":{"type":"LineString","coordinates":[[1.5,2.0],[1.6,2.1,30]]}}"#,
            "\r\n",
            r#"{"type":"Reference","location":"multi/4/5.json"}"#,
            "\n",
            r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"GeometryCollection","geometries":[{"type":"Point","coordinates":["1","2"]}]},"properties":null}]}"#,
        );
        let objects = parse_geojson_lines(text.as_bytes()).unwrap();
        assert_eq!(objects.len(), 3);
        let GeoJsonObject::Feature(feature) = &objects[0] else { panic!("{:?}", objects[0]) };
        assert_eq!(feature.id.as_deref(), Some("way/1"));
        let tags = feature.properties.as_ref().unwrap();
        assert_eq!((tags["highway"].as_str(), tags["lanes"].as_str()), ("primary", "3"));
        assert_eq!(feature.geometry, Geometry::LineString(vec![GeoPoint { lon: 1.5, lat: 2.0 }, GeoPoint { lon: 1.6, lat: 2.1 }]));
        assert_eq!(objects[1], GeoJsonObject::Reference("multi/4/5.json".into()));
        let GeoJsonObject::FeatureCollection(features) = &objects[2] else { panic!() };
        assert!(features[0].properties.is_none());
    }

    #[test]
    fn refuses_what_the_upstream_deserializer_refuses() {
        for line in [
            r#"{"coordinates":[1,2],"type":"Point"}"#,
            r#"{"type":"Point","coordinates":[1,2],"bbox":[0,0,1,1]}"#,
            r#"{"type":"LineString","coordinates":[[1,2]]}"#,
            r#"{"type":"Polygon","coordinates":[[[0,0],[1,0],[1,1],[0,0.5]]]}"#,
            r#"{"type":"Polygon","coordinates":[]}"#,
            r#"{"type":"Feature","properties":{}}"#,
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"bbox":1}"#,
            r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"a":true}}"#,
            r#"{"type":"Point","coordinates":[1,2,"3"]}"#,
            r#"{"type":"Circle","coordinates":[1,2]}"#,
            r#"{"type":"Point","coordinates":[1,2]} {}"#,
            "   ",
        ] {
            assert!(parse_geojson(line).is_err(), "{line}");
        }
        assert!(parse_geojson_lines(b"{\"type\":\"Point\",\"coordinates\":[0,0]}\n\n{\"type\":\"Point\",\"coordinates\":[0,0]}").is_err());
    }

    #[test]
    fn rings_compare_coordinates_like_double_compare() {
        let ring = |last: f64| format!(r#"{{"type":"Polygon","coordinates":[[[{last},0],[1,0],[1,1],[0,1],[0,0]]]}}"#);
        assert!(parse_geojson(&ring(0.0)).is_ok());
        assert!(parse_geojson(&ring(-0.0)).is_err(), "-0.0 and 0.0 differ under Double.compare");
    }

    #[test]
    fn splits_lines_like_buffered_reader() {
        let lines: Vec<&str> = java_lines("a\r\nb\rc\n\nd\n").collect();
        assert_eq!(lines, vec!["a", "b", "c", "", "d"]);
    }
}
