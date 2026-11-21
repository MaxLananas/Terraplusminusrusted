//! Loads elevation layer configs in TerraMinusMinus' `heights.json5` schema.

use crate::blend::BlendMode;
use crate::bounds::AxisBounds;
use crate::condition::DoubleCondition;
use crate::dataset::{TileFormat, TiledScalarDataset};
use crate::error::ElevationError;
use crate::layered::{LayeredScalarDataset, ScalarLayer};
use std::sync::Arc;
use terra_codec::{JsonValue, parse_relaxed_json};
use terra_projection::{SharedProjection, WebMercatorProjection};

/// What to do with layers whose tile format this implementation cannot decode (GeoTIFF).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedLayerPolicy {
    /// Fail loading: the config cannot be honoured in full.
    Reject,
    /// Drop such layers and report them in [`LoadedLayers::skipped`].
    Skip,
}

#[derive(Debug, Clone)]
pub struct LoadedLayers {
    pub dataset: LayeredScalarDataset,
    /// One description per dropped layer (first URL template and reason).
    pub skipped: Vec<String>,
}

/// The two world-wide AWS Terrain Tiles layers of the upstream default `heights.json5`, verbatim.
pub const DEFAULT_WORLD_HEIGHTS: &str = include_str!("../resources/heights_world.json5");

pub fn load_heights_config(text: &str, policy: UnsupportedLayerPolicy) -> Result<LoadedLayers, ElevationError> {
    let document = parse_relaxed_json(text).map_err(|error| invalid(format!("heights config: {error}")))?;
    let mut layers = Vec::new();
    let mut skipped = Vec::new();
    for (index, entry) in document.as_array_lenient().iter().enumerate() {
        let context = |message: String| invalid(format!("layer {index}: {message}"));
        let dataset = entry.get("dataset").ok_or_else(|| context("missing `dataset`".into()))?;
        let format = parse_format(dataset).map_err(context)?;
        let urls = parse_urls(dataset).map_err(context)?;
        if format == TileFormat::Tiff {
            match policy {
                UnsupportedLayerPolicy::Reject => {
                    return Err(ElevationError::UnsupportedFormat("parse_tiff layers (GeoTIFF) are not supported"));
                }
                UnsupportedLayerPolicy::Skip => {
                    skipped.push(format!("layer {index} ({}): GeoTIFF unsupported", urls.first().map_or("", String::as_str)));
                    continue;
                }
            }
        }
        let projection = parse_projection(dataset).map_err(context)?;
        let resolution = required_number(dataset, "resolution").map_err(context)?;
        if resolution.fract() != 0.0 || !(1.0..=f64::from(u32::MAX)).contains(&resolution) {
            return Err(context(format!("resolution {resolution} is not a positive integer")));
        }
        let blend = parse_blend(dataset).map_err(context)?;
        if entry.get("zooms").is_none() {
            return Err(context("missing `zooms`".into()));
        }
        let bounds = entry.get("bounds").ok_or_else(|| context("missing `bounds`".into()))?;
        let priority = match entry.get("priority") {
            None => 0.0,
            Some(value) => value.as_f64().ok_or_else(|| context("`priority` is not a number".into()))?,
        };
        let condition = match entry.get("condition") {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(parse_condition(value, 0).map_err(context)?),
        };
        let id = format!("layer-{index}");
        let tiled = Arc::new(TiledScalarDataset::new(id, urls, projection, resolution as u32, blend, format)?);
        for bound in bounds.as_array_lenient() {
            layers.push(ScalarLayer { dataset: Arc::clone(&tiled), bounds: parse_bounds(bound).map_err(context)?, priority, condition: condition.clone() });
        }
    }
    if layers.is_empty() {
        return Err(invalid("heights config defines no usable layers".into()));
    }
    Ok(LoadedLayers { dataset: LayeredScalarDataset::new(layers), skipped })
}

fn invalid(message: String) -> ElevationError {
    ElevationError::InvalidConfiguration(message)
}

fn required_number(value: &JsonValue, key: &str) -> Result<f64, String> {
    value.get(key).and_then(JsonValue::as_f64).ok_or_else(|| format!("missing numeric `{key}`"))
}

/// A Jackson polymorphic wrapper: an object with exactly one member naming the type.
fn single_member(value: &JsonValue) -> Result<(&str, &JsonValue), String> {
    match value.as_object() {
        Some([(name, inner)]) => Ok((name, inner)),
        _ => Err("expected an object with exactly one type member".into()),
    }
}

fn parse_urls(dataset: &JsonValue) -> Result<Vec<String>, String> {
    let urls = dataset.get("urls").ok_or("missing `urls`")?;
    urls.as_array_lenient().iter().map(|url| url.as_str().map(str::to_owned).ok_or_else(|| "`urls` must be strings".to_owned())).collect()
}

fn parse_format(dataset: &JsonValue) -> Result<TileFormat, String> {
    let (name, _) = single_member(dataset.get("parse").ok_or("missing `parse`")?)?;
    match name {
        "parse_png_terrarium" => Ok(TileFormat::TerrariumPng),
        "parse_tiff" => Ok(TileFormat::Tiff),
        other => Err(format!("unsupported parser `{other}`")),
    }
}

fn parse_projection(dataset: &JsonValue) -> Result<SharedProjection, String> {
    let (name, settings) = single_member(dataset.get("projection").ok_or("missing `projection`")?)?;
    match name {
        "web_mercator" => {
            let zoom = required_number(settings, "zoom")?;
            if zoom.fract() != 0.0 || !(0.0..=30.0).contains(&zoom) {
                return Err(format!("invalid web_mercator zoom {zoom}"));
            }
            WebMercatorProjection::new(zoom as u32).map(|projection| Arc::new(projection) as SharedProjection).map_err(str::to_owned)
        }
        other => Err(format!("unsupported dataset projection `{other}`")),
    }
}

fn parse_blend(dataset: &JsonValue) -> Result<BlendMode, String> {
    match dataset.get("blend").and_then(JsonValue::as_str) {
        Some("NEAR") => Ok(BlendMode::Near),
        Some("LINEAR") => Ok(BlendMode::Linear),
        Some("CUBIC") => Ok(BlendMode::Cubic),
        Some(other) => Err(format!("unknown blend mode `{other}`")),
        None => Err("missing `blend`".into()),
    }
}

fn parse_bounds(value: &JsonValue) -> Result<AxisBounds, String> {
    Ok(AxisBounds::of(required_number(value, "minX")?, required_number(value, "maxX")?, required_number(value, "minZ")?, required_number(value, "maxZ")?))
}

fn parse_condition(value: &JsonValue, depth: usize) -> Result<DoubleCondition, String> {
    if depth > 32 {
        return Err("condition nesting too deep".into());
    }
    let (name, inner) = single_member(value)?;
    let number = || inner.as_f64().ok_or_else(|| format!("`{name}` expects a number"));
    let list = || inner.as_array_lenient().iter().map(|item| parse_condition(item, depth + 1)).collect::<Result<Vec<_>, _>>();
    Ok(match name {
        "equal" => DoubleCondition::Equal(number()?),
        "greater_than" => DoubleCondition::GreaterThan(number()?),
        "less_than" => DoubleCondition::LessThan(number()?),
        "and" => DoubleCondition::And(list()?),
        "or" => DoubleCondition::Or(list()?),
        "not" => DoubleCondition::Not(Box::new(parse_condition(inner.get("delegate").ok_or("`not` requires `delegate`")?, depth + 1)?)),
        other => Err(format!("unknown condition `{other}`"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_world_layers_load() {
        let loaded = load_heights_config(DEFAULT_WORLD_HEIGHTS, UnsupportedLayerPolicy::Reject).unwrap();
        assert!(loaded.skipped.is_empty());
        let layers = loaded.dataset.layers();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[1].condition, Some(DoubleCondition::LessThan(1.0)));
        assert_eq!(layers[1].priority, 1.0);
    }

    #[test]
    fn tiff_layers_are_never_silently_dropped() {
        let config = r#"[{"dataset": {"urls": ["https://example.invalid/${x}/${z}.tiff"], "projection": {"web_mercator": {"zoom": 14}},
            "resolution": 256, "blend": "NEAR", "parse": {"parse_tiff": {}}}, "bounds": [], "zooms": {"min": 0, "max": 3}}]"#;
        assert!(matches!(load_heights_config(config, UnsupportedLayerPolicy::Reject), Err(ElevationError::UnsupportedFormat(_))));
        assert!(matches!(load_heights_config(config, UnsupportedLayerPolicy::Skip), Err(ElevationError::InvalidConfiguration(_))));
    }

    #[test]
    fn nested_conditions_and_multiple_bounds() {
        let config = r#"[{"dataset": {"urls": "mirror://a/${x}/${z}.png", "projection": {"web_mercator": {"zoom": 3}},
            "resolution": 256, "blend": "LINEAR", "parse": {"parse_png_terrarium": {}}},
            "bounds": [{"minX": 1, "maxX": 0, "minZ": 0, "maxZ": 1}, {"minX": 5, "maxX": 6, "minZ": 5, "maxZ": 6}],
            "zooms": {"min": 0, "max": 0}, "condition": {"and": [{"not": {"delegate": {"equal": 0}}}, {"greater_than": -5}]}}]"#;
        let loaded = load_heights_config(config, UnsupportedLayerPolicy::Reject).unwrap();
        let layers = loaded.dataset.layers();
        assert_eq!(layers.len(), 2);
        assert_eq!((layers[0].bounds.min_x, layers[0].bounds.max_x), (0.0, 1.0));
        let condition = layers[0].condition.as_ref().unwrap();
        assert!(condition.test(3.0) && !condition.test(0.0) && !condition.test(-6.0));
    }
}
