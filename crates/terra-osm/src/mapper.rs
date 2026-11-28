//! The `osm.json5` mapping language (`OSMMapper`, `MatchCondition`, `DValue`, `DrawFunction`):
//! parsed with the shapes the upstream Gson adapters accept, evaluated as `apply` does.

use crate::bounds::Bounds2d;
use crate::draw::{BlockState, DrawFunction, PropertyValue, parse_identifier};
use crate::geojson::{GeoPoint, GeoPolygon, Geometry, Tags};
use crate::geometry::{LineKind, Point, ProjectedPolygon, VectorGeometry};
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use terra_codec::{JsonValue, parse_java_double, parse_relaxed_json_with_duplicate_keys};
use terra_elevation::numeric::{java_max, java_min};

/// TerraMinusMinus' bundled `dataset/osm/osm.json5`, which TerraPlusMinus extracts as its default.
pub const DEFAULT_OSM_CONFIG: &str = include_str!("../resources/osm.json5");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapperError(pub String);

impl fmt::Display for MapperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "osm.json5: {}", self.0)
    }
}

impl std::error::Error for MapperError {}

fn error<T>(message: impl Into<String>) -> Result<T, MapperError> {
    Err(MapperError(message.into()))
}

type Members<'a> = &'a [(String, JsonValue)];

fn members<'a>(value: &'a JsonValue, context: &str) -> Result<Members<'a>, MapperError> {
    value.as_object().ok_or_else(|| MapperError(format!("{context} must be an object")))
}

/// `Typed.read` inside `beginObject`/`endObject`: an object holding exactly one `type: value`.
fn single<'a>(value: &'a JsonValue, context: &str) -> Result<(&'a str, &'a JsonValue), MapperError> {
    match members(value, context)? {
        [(name, value)] => Ok((name.as_str(), value)),
        other => error(format!("{context} must hold exactly one typed entry, found {}", other.len())),
    }
}

fn empty_object(value: &JsonValue, context: &str) -> Result<(), MapperError> {
    if members(value, context)?.is_empty() { Ok(()) } else { error(format!("{context} takes no properties")) }
}

/// Gson `nextInt`: an integral number in `int` range.
fn int(value: &JsonValue, context: &str) -> Result<i32, MapperError> {
    let number = match value {
        JsonValue::Number(number) => *number,
        JsonValue::String(text) => parse_java_double(text).ok_or_else(|| MapperError(format!("{context} must be an integer")))?,
        _ => return error(format!("{context} must be an integer")),
    };
    if number.fract() != 0.0 || !(f64::from(i32::MIN)..=f64::from(i32::MAX)).contains(&number) {
        return error(format!("{context} must be an integer, not {number}"));
    }
    Ok(number as i32)
}

/// Gson lenient `nextDouble`.
fn double(value: &JsonValue, context: &str) -> Result<f64, MapperError> {
    match value {
        JsonValue::Number(number) => Ok(*number),
        JsonValue::String(text) => parse_java_double(text).ok_or_else(|| MapperError(format!("{context} must be a number"))),
        _ => error(format!("{context} must be a number")),
    }
}

fn string<'a>(value: &'a JsonValue, context: &str) -> Result<&'a str, MapperError> {
    value.as_str().ok_or_else(|| MapperError(format!("{context} must be a string")))
}

fn strings(value: &JsonValue, context: &str) -> Result<Vec<String>, MapperError> {
    match value {
        JsonValue::Array(items) => items.iter().map(|item| string(item, context).map(str::to_owned)).collect(),
        _ => error(format!("{context} must be an array of strings")),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DValue {
    Constant(f64),
    Tag { key: String, fallback: f64 },
    Add(Box<DValue>, Box<DValue>),
    Subtract(Box<DValue>, Box<DValue>),
    Multiply(Box<DValue>, Box<DValue>),
    Divide(Box<DValue>, Box<DValue>),
    FloorDiv(Box<DValue>, Box<DValue>),
    Min(Box<DValue>, Box<DValue>),
    Max(Box<DValue>, Box<DValue>),
}

impl DValue {
    pub fn apply(&self, tags: &Tags) -> f64 {
        match self {
            Self::Constant(value) => *value,
            Self::Tag { key, fallback } => tags.get(key).and_then(|value| parse_java_double(value)).unwrap_or(*fallback),
            Self::Add(a, b) => a.apply(tags) + b.apply(tags),
            Self::Subtract(a, b) => a.apply(tags) - b.apply(tags),
            Self::Multiply(a, b) => a.apply(tags) * b.apply(tags),
            Self::Divide(a, b) => a.apply(tags) / b.apply(tags),
            Self::FloorDiv(a, b) => (a.apply(tags) / b.apply(tags)).floor(),
            Self::Min(a, b) => java_min(a.apply(tags), b.apply(tags)),
            Self::Max(a, b) => java_max(a.apply(tags), b.apply(tags)),
        }
    }

    fn parse_typed(kind: &str, value: &JsonValue) -> Result<Self, MapperError> {
        let pair = |value: &JsonValue| -> Result<(Box<DValue>, Box<DValue>), MapperError> {
            match members(value, kind)? {
                [(first_kind, first), (second_kind, second)] => Ok((Box::new(Self::parse_typed(first_kind, first)?), Box::new(Self::parse_typed(second_kind, second)?))),
                other => error(format!("`{kind}` takes exactly two operands, found {}", other.len())),
            }
        };
        Ok(match kind {
            "constant" => Self::Constant(double(value, "constant")?),
            "tag" => {
                let mut key = None;
                let mut fallback = 0.0;
                for (name, member) in members(value, "dvalue tag")? {
                    match name.as_str() {
                        "key" => key = Some(string(member, "tag key")?.to_owned()),
                        "fallback" => fallback = double(member, "tag fallback")?,
                        other => return error(format!("invalid property: {other}")),
                    }
                }
                Self::Tag { key: key.ok_or_else(|| MapperError("dvalue tag needs a key".into()))?, fallback }
            }
            "+" | "-" | "*" | "/" | "floor_div" | "min" | "max" => {
                let (a, b) = pair(value)?;
                match kind {
                    "+" => Self::Add(a, b),
                    "-" => Self::Subtract(a, b),
                    "*" => Self::Multiply(a, b),
                    "/" => Self::Divide(a, b),
                    "floor_div" => Self::FloorDiv(a, b),
                    "min" => Self::Min(a, b),
                    _ => Self::Max(a, b),
                }
            }
            other => return error(format!("type \"{other}\" is not supported by \"dvalue\"")),
        })
    }

    fn parse(value: &JsonValue) -> Result<Self, MapperError> {
        let (kind, inner) = single(value, "dvalue")?;
        Self::parse_typed(kind, inner)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchCondition {
    False,
    And(Vec<MatchCondition>),
    Or(Vec<MatchCondition>),
    Not(Box<MatchCondition>),
    IdExactly(String),
    IdAny(HashSet<String>),
    TagPresent(String),
    TagExactly(String, String),
    TagAny(String, HashSet<String>),
    /// Raw `minX`/`maxX`/`minZ`/`maxZ` in degrees, compared as given.
    Intersects(Bounds2d),
}

impl MatchCondition {
    pub fn test(&self, id: Option<&str>, tags: &Tags, original: &Geometry) -> bool {
        match self {
            Self::False => false,
            Self::And(conditions) => conditions.iter().all(|condition| condition.test(id, tags, original)),
            Self::Or(conditions) => conditions.iter().any(|condition| condition.test(id, tags, original)),
            Self::Not(condition) => !condition.test(id, tags, original),
            Self::IdExactly(expected) => id == Some(expected.as_str()),
            Self::IdAny(expected) => id.is_some_and(|id| expected.contains(id)),
            Self::TagPresent(key) => tags.contains_key(key),
            Self::TagExactly(key, value) => tags.get(key) == Some(value),
            Self::TagAny(key, values) => tags.get(key).is_some_and(|value| values.contains(value)),
            Self::Intersects(area) => geometry_bounds(original).is_some_and(|bounds| area.intersects(&bounds)),
        }
    }

    fn parse_typed(kind: &str, value: &JsonValue) -> Result<Self, MapperError> {
        let list = |value: &JsonValue| -> Result<Vec<MatchCondition>, MapperError> {
            members(value, kind)?.iter().map(|(name, member)| Self::parse_typed(name, member)).collect()
        };
        Ok(match kind {
            "and" => Self::And(list(value)?),
            "or" => Self::Or(list(value)?),
            "not" => {
                let (inner_kind, inner) = single(value, "not")?;
                Self::Not(Box::new(Self::parse_typed(inner_kind, inner)?))
            }
            "id" => match value {
                JsonValue::Array(_) => {
                    let ids: HashSet<String> = strings(value, "id")?.into_iter().collect();
                    match ids.len() {
                        0 => Self::False,
                        1 => Self::IdExactly(ids.into_iter().next().unwrap_or_default()),
                        _ => Self::IdAny(ids),
                    }
                }
                _ => Self::IdExactly(string(value, "id")?.to_owned()),
            },
            "tag" => {
                let (key, expected) = match members(value, "tag")? {
                    [(key, expected)] => (key.clone(), expected),
                    other => return error(format!("tag match takes exactly one key, found {}", other.len())),
                };
                match expected {
                    JsonValue::Null => Self::TagPresent(key),
                    JsonValue::Array(_) => {
                        let values: HashSet<String> = strings(expected, "tag values")?.into_iter().collect();
                        match values.len() {
                            0 => Self::False,
                            1 => Self::TagExactly(key, values.into_iter().next().unwrap_or_default()),
                            _ => Self::TagAny(key, values),
                        }
                    }
                    _ => Self::TagExactly(key, string(expected, "tag value")?.to_owned()),
                }
            }
            "intersects" => {
                let mut area = Bounds2d { min_x: 0.0, max_x: 0.0, min_z: 0.0, max_z: 0.0 };
                for (name, member) in members(value, "intersects")? {
                    let number = double(member, name)?;
                    match name.as_str() {
                        "minX" => area.min_x = number,
                        "maxX" => area.max_x = number,
                        "minZ" => area.min_z = number,
                        "maxZ" => area.max_z = number,
                        other => return error(format!("invalid property: {other}")),
                    }
                }
                Self::Intersects(area)
            }
            other => return error(format!("type \"{other}\" is not supported by \"match\"")),
        })
    }
}

fn draw_function(kind: &str, value: &JsonValue) -> Result<DrawFunction, MapperError> {
    let weighted = |value: &JsonValue, defaults: (i32, i32)| -> Result<(Box<DrawFunction>, i32, i32), MapperError> {
        let mut delegate = None;
        let (mut first, mut second) = defaults;
        for (name, member) in members(value, kind)? {
            match (kind, name.as_str()) {
                (_, "delegate") => {
                    let (inner_kind, inner) = single(member, "delegate")?;
                    delegate = Some(Box::new(draw_function(inner_kind, inner)?));
                }
                ("weight_clamp", "min") | ("weight_add" | "weight_greater_than" | "weight_less_than", "value") => first = int(member, name)?,
                ("weight_clamp", "max") => second = int(member, name)?,
                (_, other) => return error(format!("invalid property: {other}")),
            }
        }
        Ok((delegate.ok_or_else(|| MapperError(format!("{kind} needs a delegate")))?, first, second))
    };
    Ok(match kind {
        "all" => DrawFunction::All(members(value, "all")?.iter().map(|(name, member)| draw_function(name, member)).collect::<Result<_, _>>()?),
        "weight_add" => {
            let (delegate, value, _) = weighted(value, (0, 0))?;
            DrawFunction::WeightAdd { delegate, value }
        }
        "weight_clamp" => {
            let (delegate, min, max) = weighted(value, (i32::MIN, i32::MAX))?;
            DrawFunction::WeightClamp { delegate, min, max }
        }
        "weight_greater_than" => {
            let (delegate, value, _) = weighted(value, (0, 0))?;
            DrawFunction::WeightGreaterThan { delegate, value }
        }
        "weight_less_than" => {
            let (delegate, value, _) = weighted(value, (0, 0))?;
            DrawFunction::WeightLessThan { delegate, value }
        }
        "block" => DrawFunction::Block(block_state(value)?),
        "no_trees" => empty_object(value, kind).map(|()| DrawFunction::NoTrees)?,
        "ocean" => empty_object(value, kind).map(|()| DrawFunction::Ocean)?,
        "water" => empty_object(value, kind).map(|()| DrawFunction::Water)?,
        other => return error(format!("type \"{other}\" is not supported by \"draw\"")),
    })
}

/// `BlockStateParser`.
fn block_state(value: &JsonValue) -> Result<BlockState, MapperError> {
    let mut id = None;
    let mut properties: Vec<(String, PropertyValue)> = Vec::new();
    for (name, member) in members(value, "block")? {
        match name.as_str() {
            "id" => id = Some(parse_identifier(string(member, "block id")?).map_err(MapperError)?),
            "properties" => {
                for (property, setting) in members(member, "block properties")? {
                    if property.is_empty() {
                        return error("property name cannot be empty");
                    }
                    let setting = match setting {
                        JsonValue::String(text) if !text.is_empty() => PropertyValue::String(text.clone()),
                        JsonValue::String(_) => return error("property value cannot be empty"),
                        JsonValue::Number(_) => PropertyValue::Int(int(setting, property)?),
                        JsonValue::Bool(flag) => PropertyValue::Bool(*flag),
                        _ => return error(format!("invalid property type for {property}")),
                    };
                    properties.retain(|(existing, _)| existing != property);
                    properties.push((property.clone(), setting));
                }
            }
            other => return error(format!("Invalid block state: {other}")),
        }
    }
    properties.sort_by(|a, b| a.0.cmp(&b.0));
    let id = id.ok_or_else(|| MapperError("Trying to build a block state, but no block is set".into()))?;
    Ok(BlockState { id, properties })
}

fn parse_draw(value: &JsonValue) -> Result<Arc<DrawFunction>, MapperError> {
    let (kind, inner) = single(value, "draw")?;
    draw_function(kind, inner).map(Arc::new)
}

#[derive(Debug, Clone, PartialEq)]
pub enum LineMapper {
    All(Vec<LineMapper>),
    Any(Vec<LineMapper>),
    First(Vec<LineMapper>),
    Condition { condition: MatchCondition, emit: Box<LineMapper> },
    Nothing,
    Narrow { draw: Arc<DrawFunction>, layer: DValue },
    Sharp { draw: Arc<DrawFunction>, layer: DValue },
    Wide { draw: Arc<DrawFunction>, layer: DValue, radius: DValue },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PolygonMapper {
    All(Vec<PolygonMapper>),
    Any(Vec<PolygonMapper>),
    First(Vec<PolygonMapper>),
    Condition { condition: MatchCondition, emit: Box<PolygonMapper> },
    Nothing,
    ConvertToLines(Box<LineMapper>),
    Distance { draw: Arc<DrawFunction>, layer: DValue, max_dist: i32 },
    Fill { draw: Arc<DrawFunction>, layer: DValue },
}

/// Mapper output: `None` is upstream's `null` ("did not match"), which stops `all` and lets
/// `first` try its next child; `Some(empty)` matched and emitted nothing.
pub type Emitted = Result<Option<Vec<Arc<VectorGeometry>>>, MapperError>;

fn child_id(id: Option<&str>, index: usize) -> String {
    format!("{}/{index}", id.unwrap_or("null"))
}

fn require_id(id: Option<&str>) -> Result<String, MapperError> {
    id.map(str::to_owned).ok_or_else(|| MapperError("an element without an id reached a geometry (upstream fails the tile)".into()))
}

/// Shared parsing of `all`/`any`/`first`/`condition`/`nothing`, generic over line and polygon.
trait Combinator: Sized {
    const NAME: &'static str;
    fn all(children: Vec<Self>) -> Self;
    fn any(children: Vec<Self>) -> Self;
    fn first(children: Vec<Self>) -> Self;
    fn condition(condition: MatchCondition, emit: Self) -> Self;
    fn nothing() -> Self;
    fn leaf(kind: &str, value: &JsonValue) -> Result<Self, MapperError>;

    fn parse_typed(kind: &str, value: &JsonValue) -> Result<Self, MapperError> {
        let children = |value: &JsonValue| -> Result<Vec<Self>, MapperError> {
            let parsed: Vec<Self> = members(value, kind)?.iter().map(|(name, member)| Self::parse_typed(name, member)).collect::<Result<_, _>>()?;
            if parsed.is_empty() { error(format!("`{kind}` needs at least one member")) } else { Ok(parsed) }
        };
        let unwrap_single = |mut parsed: Vec<Self>, wrap: fn(Vec<Self>) -> Self| if parsed.len() == 1 { parsed.pop().map_or_else(Self::nothing, |only| only) } else { wrap(parsed) };
        match kind {
            "all" => Ok(unwrap_single(children(value)?, Self::all)),
            "any" => Ok(unwrap_single(children(value)?, Self::any)),
            "first" => Ok(unwrap_single(children(value)?, Self::first)),
            "nothing" => empty_object(value, kind).map(|()| Self::nothing()),
            "condition" => {
                let mut condition = None;
                let mut emit = None;
                for (name, member) in members(value, "condition")? {
                    match name.as_str() {
                        "match" => {
                            let (match_kind, inner) = single(member, "match")?;
                            condition = Some(MatchCondition::parse_typed(match_kind, inner)?);
                        }
                        "emit" => {
                            let (emit_kind, inner) = single(member, "emit")?;
                            emit = Some(Self::parse_typed(emit_kind, inner)?);
                        }
                        other => return error(format!("invalid property: {other}")),
                    }
                }
                match (condition, emit) {
                    (Some(condition), Some(emit)) => Ok(Self::condition(condition, emit)),
                    _ => error("condition needs both match and emit"),
                }
            }
            other => Self::leaf(other, value),
        }
    }
}

struct LeafFields {
    draw: Option<Arc<DrawFunction>>,
    layer: Option<DValue>,
    radius: Option<DValue>,
    max_dist: Option<i32>,
}

fn leaf_fields(kind: &str, value: &JsonValue, allowed: &[&str]) -> Result<LeafFields, MapperError> {
    let mut fields = LeafFields { draw: None, layer: None, radius: None, max_dist: None };
    for (name, member) in members(value, kind)? {
        if !allowed.contains(&name.as_str()) {
            return error(format!("invalid property: {name}"));
        }
        match name.as_str() {
            "draw" => fields.draw = Some(parse_draw(member)?),
            "layer" => fields.layer = Some(DValue::parse(member)?),
            "radius" => fields.radius = Some(DValue::parse(member)?),
            _ => fields.max_dist = Some(int(member, "maxDist")?),
        }
    }
    if fields.draw.is_none() || fields.layer.is_none() || (allowed.contains(&"radius") && fields.radius.is_none()) {
        return error(format!("`{kind}` needs {}", allowed.iter().filter(|name| **name != "maxDist").copied().collect::<Vec<_>>().join(", ")));
    }
    Ok(fields)
}

impl Combinator for LineMapper {
    const NAME: &'static str = "line";
    fn all(children: Vec<Self>) -> Self {
        Self::All(children)
    }
    fn any(children: Vec<Self>) -> Self {
        Self::Any(children)
    }
    fn first(children: Vec<Self>) -> Self {
        Self::First(children)
    }
    fn condition(condition: MatchCondition, emit: Self) -> Self {
        Self::Condition { condition, emit: Box::new(emit) }
    }
    fn nothing() -> Self {
        Self::Nothing
    }
    fn leaf(kind: &str, value: &JsonValue) -> Result<Self, MapperError> {
        let fields = match kind {
            "narrow" | "sharp" => leaf_fields(kind, value, &["draw", "layer"])?,
            "wide" => leaf_fields(kind, value, &["draw", "layer", "radius"])?,
            other => return error(format!("type \"{other}\" is not supported by \"{}\"", Self::NAME)),
        };
        let (Some(draw), Some(layer)) = (fields.draw, fields.layer) else { return error(format!("`{kind}` needs draw and layer")) };
        Ok(match (kind, fields.radius) {
            ("narrow", _) => Self::Narrow { draw, layer },
            ("sharp", _) => Self::Sharp { draw, layer },
            (_, Some(radius)) => Self::Wide { draw, layer, radius },
            _ => return error("`wide` needs a radius"),
        })
    }
}

impl Combinator for PolygonMapper {
    const NAME: &'static str = "polygon";
    fn all(children: Vec<Self>) -> Self {
        Self::All(children)
    }
    fn any(children: Vec<Self>) -> Self {
        Self::Any(children)
    }
    fn first(children: Vec<Self>) -> Self {
        Self::First(children)
    }
    fn condition(condition: MatchCondition, emit: Self) -> Self {
        Self::Condition { condition, emit: Box::new(emit) }
    }
    fn nothing() -> Self {
        Self::Nothing
    }
    fn leaf(kind: &str, value: &JsonValue) -> Result<Self, MapperError> {
        match kind {
            "convert" => {
                let (target, inner) = single(value, "convert")?;
                if target != "line" {
                    return error(format!("type \"{target}\" is not supported by \"convert\""));
                }
                let (line_kind, line_value) = single(inner, "convert.line")?;
                Ok(Self::ConvertToLines(Box::new(LineMapper::parse_typed(line_kind, line_value)?)))
            }
            "fill" | "distance" => {
                let allowed: &[&str] = if kind == "fill" { &["draw", "layer"] } else { &["draw", "layer", "maxDist"] };
                let fields = leaf_fields(kind, value, allowed)?;
                let (Some(draw), Some(layer)) = (fields.draw, fields.layer) else { return error(format!("`{kind}` needs draw and layer")) };
                if kind == "fill" {
                    return Ok(Self::Fill { draw, layer });
                }
                let max_dist = fields.max_dist.unwrap_or(2);
                if max_dist <= 0 {
                    return error(format!("maxDist ({max_dist}) must be positive"));
                }
                Ok(Self::Distance { draw, layer, max_dist })
            }
            other => error(format!("type \"{other}\" is not supported by \"{}\"", Self::NAME)),
        }
    }
}

fn apply_children<M>(children: &[M], id: Option<&str>, stop_on_unmatched: bool, apply: impl Fn(&M, Option<&str>) -> Emitted) -> Emitted {
    let mut out = Vec::new();
    for (index, child) in children.iter().enumerate() {
        let child_id = child_id(id, index);
        match apply(child, Some(&child_id))? {
            Some(emitted) => out.extend(emitted),
            None if stop_on_unmatched => return Ok(None),
            None => {}
        }
    }
    Ok(Some(out))
}

impl LineMapper {
    pub fn apply(&self, id: Option<&str>, tags: &Tags, original: &Geometry, lines: &[Vec<Point>]) -> Emitted {
        match self {
            Self::All(children) => apply_children(children, id, true, |child, id| child.apply(id, tags, original, lines)),
            Self::Any(children) => apply_children(children, id, false, |child, id| child.apply(id, tags, original, lines)),
            Self::First(children) => {
                for child in children {
                    if let Some(emitted) = child.apply(id, tags, original, lines)? {
                        return Ok(Some(emitted));
                    }
                }
                Ok(None)
            }
            Self::Condition { condition, emit } => {
                if condition.test(id, tags, original) {
                    emit.apply(id, tags, original, lines)
                } else {
                    Ok(None)
                }
            }
            Self::Nothing => Ok(Some(Vec::new())),
            Self::Narrow { draw, layer } => Ok(Some(vec![Arc::new(VectorGeometry::line(LineKind::Narrow, require_id(id)?, layer.apply(tags), Arc::clone(draw), lines))])),
            Self::Sharp { draw, layer } => Ok(Some(vec![Arc::new(VectorGeometry::line(LineKind::Sharp, require_id(id)?, layer.apply(tags), Arc::clone(draw), lines))])),
            Self::Wide { draw, layer, radius } => {
                Ok(Some(vec![Arc::new(VectorGeometry::wide_line(require_id(id)?, layer.apply(tags), Arc::clone(draw), lines, radius.apply(tags)))]))
            }
        }
    }

    fn collect_blocks<'a>(&'a self, out: &mut Vec<&'a BlockState>) {
        match self {
            Self::All(children) | Self::Any(children) | Self::First(children) => children.iter().for_each(|child| child.collect_blocks(out)),
            Self::Condition { emit, .. } => emit.collect_blocks(out),
            Self::Nothing => {}
            Self::Narrow { draw, .. } | Self::Sharp { draw, .. } | Self::Wide { draw, .. } => draw.collect_blocks(out),
        }
    }
}

impl PolygonMapper {
    pub fn apply(&self, id: Option<&str>, tags: &Tags, original: &Geometry, polygons: &[ProjectedPolygon]) -> Emitted {
        match self {
            Self::All(children) => apply_children(children, id, true, |child, id| child.apply(id, tags, original, polygons)),
            Self::Any(children) => apply_children(children, id, false, |child, id| child.apply(id, tags, original, polygons)),
            Self::First(children) => {
                for child in children {
                    if let Some(emitted) = child.apply(id, tags, original, polygons)? {
                        return Ok(Some(emitted));
                    }
                }
                Ok(None)
            }
            Self::Condition { condition, emit } => {
                if condition.test(id, tags, original) {
                    emit.apply(id, tags, original, polygons)
                } else {
                    Ok(None)
                }
            }
            Self::Nothing => Ok(Some(Vec::new())),
            Self::ConvertToLines(next) => {
                let mut lines = Vec::new();
                for polygon in polygons {
                    lines.push(polygon.outer.clone());
                    lines.extend(polygon.inner.iter().cloned());
                }
                next.apply(id, tags, original, &lines)
            }
            Self::Distance { draw, layer, max_dist } => {
                Ok(Some(vec![Arc::new(VectorGeometry::polygon(require_id(id)?, layer.apply(tags), Arc::clone(draw), polygons, Some(*max_dist)))]))
            }
            Self::Fill { draw, layer } => Ok(Some(vec![Arc::new(VectorGeometry::polygon(require_id(id)?, layer.apply(tags), Arc::clone(draw), polygons, None))])),
        }
    }

    fn collect_blocks<'a>(&'a self, out: &mut Vec<&'a BlockState>) {
        match self {
            Self::All(children) | Self::Any(children) | Self::First(children) => children.iter().for_each(|child| child.collect_blocks(out)),
            Self::Condition { emit, .. } => emit.collect_blocks(out),
            Self::Nothing => {}
            Self::ConvertToLines(next) => next.collect_blocks(out),
            Self::Distance { draw, .. } | Self::Fill { draw, .. } => draw.collect_blocks(out),
        }
    }
}

/// `Root`: the parsed `osm.json5`.
#[derive(Debug, Clone, PartialEq)]
pub struct OsmMapper {
    pub line: LineMapper,
    pub polygon: PolygonMapper,
}

impl OsmMapper {
    pub fn parse(text: &str) -> Result<Self, MapperError> {
        let document = parse_relaxed_json_with_duplicate_keys(text).map_err(|error| MapperError(error.to_string()))?;
        let mut line = None;
        let mut polygon = None;
        for (name, value) in members(&document, "the document")? {
            match name.as_str() {
                "line" => {
                    let (kind, inner) = single(value, "line")?;
                    line = Some(LineMapper::parse_typed(kind, inner)?);
                }
                "polygon" => {
                    let (kind, inner) = single(value, "polygon")?;
                    polygon = Some(PolygonMapper::parse_typed(kind, inner)?);
                }
                other => return error(format!("invalid property: {other}")),
            }
        }
        match (line, polygon) {
            (Some(line), Some(polygon)) => Ok(Self { line, polygon }),
            _ => error("both `line` and `polygon` are required"),
        }
    }

    pub fn default_config() -> Result<Self, MapperError> {
        Self::parse(DEFAULT_OSM_CONFIG)
    }

    /// `Root.apply`; `project` converts geographic points to block coordinates and fails for
    /// points outside the projection, which skips the element.
    pub fn apply(&self, id: Option<&str>, tags: &Tags, original: &Geometry, project: &dyn Fn(GeoPoint) -> Option<Point>) -> Emitted {
        let project_line = |points: &[GeoPoint]| points.iter().map(|point| project(*point)).collect::<Option<Vec<Point>>>();
        let project_polygon = |polygon: &GeoPolygon| -> Option<ProjectedPolygon> {
            Some(ProjectedPolygon { outer: project_line(&polygon.outer)?, inner: polygon.inner.iter().map(|ring| project_line(ring)).collect::<Option<_>>()? })
        };
        match original {
            Geometry::Point(_) | Geometry::MultiPoint(_) => Ok(None),
            Geometry::LineString(points) => match project_line(points) {
                Some(line) => self.line.apply(id, tags, original, &[line]),
                None => Ok(None),
            },
            Geometry::MultiLineString(lines) => match lines.iter().map(|line| project_line(line)).collect::<Option<Vec<_>>>() {
                Some(lines) => self.line.apply(id, tags, original, &lines),
                None => Ok(None),
            },
            Geometry::Polygon(polygon) => match project_polygon(polygon) {
                Some(projected) => self.polygon.apply(id, tags, original, &[projected]),
                None => Ok(None),
            },
            Geometry::MultiPolygon(polygons) => match polygons.iter().map(project_polygon).collect::<Option<Vec<_>>>() {
                Some(projected) => self.polygon.apply(id, tags, original, &projected),
                None => Ok(None),
            },
            Geometry::GeometryCollection(_) => error("a GeometryCollection reached the mapper"),
        }
    }

    /// Every block state the mapping can place.
    pub fn blocks(&self) -> Vec<&BlockState> {
        let mut out = Vec::new();
        self.line.collect_blocks(&mut out);
        self.polygon.collect_blocks(&mut out);
        out
    }
}

/// `Geometry.bounds()` in degrees; `None` for empty multi-geometries.
pub fn geometry_bounds(geometry: &Geometry) -> Option<Bounds2d> {
    let points_bounds = |points: &[GeoPoint]| -> Option<Bounds2d> {
        if points.is_empty() {
            return None;
        }
        let mut bounds = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
        for point in points {
            bounds = (java_min(bounds.0, point.lon), java_max(bounds.1, point.lon), java_min(bounds.2, point.lat), java_max(bounds.3, point.lat));
        }
        Some(Bounds2d::of(bounds.0, bounds.1, bounds.2, bounds.3))
    };
    let union = |parts: Vec<Option<Bounds2d>>| -> Option<Bounds2d> {
        let mut parts = parts.into_iter();
        let first = parts.next()??;
        parts.try_fold(first, |acc, part| part.map(|part| acc.union(&part)))
    };
    match geometry {
        Geometry::Point(point) => Some(Bounds2d::of(point.lon, point.lon, point.lat, point.lat)),
        Geometry::MultiPoint(points) | Geometry::LineString(points) => points_bounds(points),
        Geometry::MultiLineString(lines) => union(lines.iter().map(|line| points_bounds(line)).collect()),
        Geometry::Polygon(polygon) => points_bounds(&polygon.outer),
        Geometry::MultiPolygon(polygons) => union(polygons.iter().map(|polygon| points_bounds(&polygon.outer)).collect()),
        Geometry::GeometryCollection(children) => union(children.iter().map(geometry_bounds).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(pairs: &[(&str, &str)]) -> Tags {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    #[test]
    fn default_config_parses_and_lists_its_blocks() {
        let mapper = OsmMapper::default_config().unwrap();
        let blocks: Vec<String> = mapper.blocks().iter().map(|block| block.to_string()).collect();
        assert_eq!(blocks, vec!["minecraft:gray_concrete", "minecraft:dirt_path", "minecraft:bricks"]);
        let LineMapper::First(rules) = &mapper.line else { panic!("{:?}", mapper.line) };
        assert_eq!(rules.len(), 10);
    }

    #[test]
    fn radius_expressions_follow_the_tags() {
        let mapper = OsmMapper::default_config().unwrap();
        let LineMapper::First(rules) = &mapper.line else { panic!() };
        let LineMapper::Condition { emit, .. } = &rules[1] else { panic!() };
        let LineMapper::Wide { radius, .. } = emit.as_ref() else { panic!() };
        assert_eq!(radius.apply(&tags(&[])), 9.0, "floor((2*3)/2) + 6");
        assert_eq!(radius.apply(&tags(&[("lanes", "5")])), 13.0);
        assert_eq!(radius.apply(&tags(&[("lanes", "2;3")])), 9.0, "unparseable tags use the fallback");
    }

    #[test]
    fn tunnels_are_matched_but_emit_nothing() {
        let mapper = OsmMapper::default_config().unwrap();
        let line = Geometry::LineString(vec![GeoPoint { lon: 0.0, lat: 0.0 }, GeoPoint { lon: 1.0, lat: 1.0 }]);
        let project = |point: GeoPoint| Some(Point { x: point.lon, z: point.lat });
        let tunnel = mapper.apply(Some("way/1"), &tags(&[("highway", "motorway"), ("tunnel", "yes")]), &line, &project).unwrap();
        assert_eq!(tunnel.map(|emitted| emitted.len()), Some(0));
        let road = mapper.apply(Some("way/1"), &tags(&[("highway", "motorway"), ("tunnel", "no")]), &line, &project).unwrap().unwrap();
        assert_eq!(road.len(), 1);
        assert!(mapper.apply(Some("way/2"), &tags(&[("amenity", "bench")]), &line, &project).unwrap().is_none());
    }

    #[test]
    fn all_and_any_number_their_children_unless_there_is_one() {
        let config = r#"{"line": {"all": {"narrow": {"draw": {"water": {}}, "layer": {"constant": 0}}, "sharp": {"draw": {"water": {}}, "layer": {"constant": 1}}}},
                         "polygon": {"any": {"fill": {"draw": {"water": {}}, "layer": {"constant": 0}}}}}"#;
        let mapper = OsmMapper::parse(config).unwrap();
        assert!(matches!(mapper.polygon, PolygonMapper::Fill { .. }), "a single child replaces its combinator");
        let line = Geometry::LineString(vec![GeoPoint { lon: 0.0, lat: 0.0 }, GeoPoint { lon: 1.0, lat: 1.0 }]);
        let emitted = mapper.apply(None, &tags(&[]), &line, &|p| Some(Point { x: p.lon, z: p.lat })).unwrap().unwrap();
        let ids: Vec<&str> = emitted.iter().map(|g| g.id()).collect();
        assert_eq!(ids, vec!["null/0", "null/1"]);
    }

    #[test]
    fn rejects_malformed_configs() {
        for config in [
            r#"{"line": {"nothing": {}}}"#,
            r#"{"line": {"nothing": {}}, "polygon": {"nothing": {}}, "extra": 1}"#,
            r#"{"line": {"first": {}}, "polygon": {"nothing": {}}}"#,
            r#"{"line": {"narrow": {"draw": {"water": {}}}}, "polygon": {"nothing": {}}}"#,
            r#"{"line": {"nothing": {}}, "polygon": {"distance": {"draw": {"water": {}}, "layer": {"constant": 0}, "maxDist": 0}}}"#,
            r#"{"line": {"nothing": {}}, "polygon": {"fill": {"draw": {"block": {"id": "Stone"}}, "layer": {"constant": 0}}}}"#,
            r#"{"line": {"condition": {"match": {"tag": {"a": null, "b": null}}, "emit": {"nothing": {}}}}, "polygon": {"nothing": {}}}"#,
            r#"{"line": {"wide": {"draw": {"water": {}}, "layer": {"+": {"constant": 1}}, "radius": {"constant": 1}}}, "polygon": {"nothing": {}}}"#,
        ] {
            assert!(OsmMapper::parse(config).is_err(), "{config}");
        }
    }
}
