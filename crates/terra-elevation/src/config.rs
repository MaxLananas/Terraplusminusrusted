//! Loads elevation layer configs in TerraMinusMinus' `heights.json5` schema.

use crate::blend::BlendMode;
use crate::bounds::AxisBounds;
use crate::condition::DoubleCondition;
use crate::dataset::{TileFormat, TiledScalarDataset};
use crate::error::ElevationError;
use crate::int_format::IntFormat;
use crate::layered::{LayeredScalarDataset, ScalarLayer};
use std::sync::Arc;
use terra_codec::{JsonValue, parse_relaxed_json};
use terra_projection::{
    AzimuthalEquidistantProjection, BteDymaxionProjection, CenteredMercatorProjection, ClampTransform, ConformalDymaxionProjection, DymaxionProjection, EqualEarthProjection,
    EquirectangularProjection, FlipHorizontalTransform, FlipVerticalTransform, LambertAzimuthalProjection, OffsetTransform, RotateTransform, ScaleTransform, SharedProjection,
    SinusoidalProjection, StereographicProjection, SwapAxesTransform, TransverseMercatorProjection, WebMercatorProjection,
};

/// What to do with layers of a valid upstream type that this implementation cannot load.
///
/// Every projection, condition and (double and integer) scalar parser of TerraMinusMinus'
/// `GlobalParseRegistries` is ported, so no layer is ever skipped today; the policy stays part of
/// the API and of the `unsupported_layers` setting so existing configurations keep loading.
/// Unknown type names are always configuration errors, whatever the policy.
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

/// The upstream default `tree_cover.json5` (treecover2000 GeoTIFFs), verbatim.
pub const DEFAULT_TREE_COVER: &str = include_str!("../resources/tree_cover.json5");

pub fn load_heights_config(text: &str, policy: UnsupportedLayerPolicy) -> Result<LoadedLayers, ElevationError> {
    load_scalar_config(text, policy, "heights", "layer")
}

/// A tree-cover dataset config: the same schema as `heights.json5`, values in `[0, 1]`.
pub fn load_tree_cover_config(text: &str, policy: UnsupportedLayerPolicy) -> Result<LoadedLayers, ElevationError> {
    load_scalar_config(text, policy, "tree cover", "tree-cover-layer")
}

/// Why a layer cannot be loaded: a mistake in the file.
enum Problem {
    Invalid(String),
}

impl From<String> for Problem {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}

impl From<&str> for Problem {
    fn from(message: &str) -> Self {
        Self::Invalid(message.to_owned())
    }
}

/// `MultiScalarDataset` configs. `id_prefix` keeps tile cache keys apart between datasets.
fn load_scalar_config(text: &str, _policy: UnsupportedLayerPolicy, kind: &str, id_prefix: &str) -> Result<LoadedLayers, ElevationError> {
    let document = parse_relaxed_json(text).map_err(|error| invalid(format!("{kind} config: {error}")))?;
    let mut layers = Vec::new();
    let skipped = Vec::new();
    for (index, entry) in document.as_array_lenient().iter().enumerate() {
        let context = |message: String| invalid(format!("layer {index}: {message}"));
        let dataset = entry.get("dataset").ok_or_else(|| context("missing `dataset`".into()))?;
        let urls = parse_urls(dataset).map_err(context)?;
        let parsed = parse_format(dataset.get("parse").ok_or("missing `parse`").map_err(|error| context(error.into()))?, 0)
            .and_then(|format| Ok((format, parse_projection(dataset.get("projection").ok_or("missing `projection`")?, 0)?)));
        let (format, projection) = match parsed {
            Ok(parsed) => parsed,
            Err(Problem::Invalid(message)) => return Err(context(message)),
        };
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
        let id = format!("{id_prefix}-{index}");
        let tiled = Arc::new(TiledScalarDataset::new(id, urls, projection, resolution as u32, blend, format)?);
        for bound in bounds.as_array_lenient() {
            layers.push(ScalarLayer { dataset: Arc::clone(&tiled), bounds: parse_bounds(bound).map_err(context)?, priority, condition: condition.clone() });
        }
    }
    if layers.is_empty() {
        return Err(invalid(format!("{kind} config defines no usable layers")));
    }
    Ok(LoadedLayers { dataset: LayeredScalarDataset::new(layers), skipped })
}

fn invalid(message: String) -> ElevationError {
    ElevationError::InvalidConfiguration(message)
}

fn required_number(value: &JsonValue, key: &str) -> Result<f64, String> {
    value.get(key).and_then(JsonValue::as_f64).ok_or_else(|| format!("missing numeric `{key}`"))
}

/// A nullable `Double` creator property: absent or `null` gives `None`, as Jackson.
fn optional_number(value: &JsonValue, key: &str) -> Result<Option<f64>, String> {
    match value.get(key) {
        None | Some(JsonValue::Null) => Ok(None),
        Some(member) => member.as_f64().map(Some).ok_or_else(|| format!("`{key}` must be a number")),
    }
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

const MAX_NESTING: usize = 16;

/// `GlobalParseRegistries.SCALAR_PARSERS_DOUBLE`.
fn parse_format(value: &JsonValue, depth: usize) -> Result<TileFormat, Problem> {
    if depth > MAX_NESTING {
        return Err("parser nesting too deep".into());
    }
    let (name, settings) = single_member(value)?;
    let delegate = || -> Result<Box<TileFormat>, Problem> {
        Ok(Box::new(parse_format(settings.get("delegate").ok_or_else(|| format!("`{name}` requires `delegate`"))?, depth + 1)?))
    };
    let number = || required_number(settings, "value").map_err(Problem::from);
    Ok(match name {
        "parse_png_terrarium" => TileFormat::TerrariumPng,
        "parse_tiff" | "parse_tiff_auto" | "parse_tiff_fp" | "parse_tiff_int" => TileFormat::Tiff,
        "add" => TileFormat::Add(delegate()?, number()?),
        "multiply" => TileFormat::Multiply(delegate()?, number()?),
        "divide" => TileFormat::Divide(delegate()?, number()?),
        "flip_x" => TileFormat::FlipX(delegate()?),
        "flip_z" => TileFormat::FlipZ(delegate()?),
        "swap_axes" => TileFormat::SwapAxes(delegate()?),
        "from_int" => TileFormat::FromInt(int_delegate(name, settings, depth)?),
        other => return Err(format!("unknown parser `{other}`").into()),
    })
}

/// A required `delegate` of a `from_int` or integer parser.
fn int_delegate(name: &str, settings: &JsonValue, depth: usize) -> Result<Box<IntFormat>, Problem> {
    Ok(Box::new(parse_int_format(settings.get("delegate").ok_or_else(|| format!("`{name}` requires `delegate`"))?, depth + 1)?))
}

/// A required primitive `int` property as Jackson binds it with its default features: integral
/// numbers in range as they are, fractions truncated toward zero (`ACCEPT_FLOAT_AS_INT`),
/// anything outside `int` refused.
fn required_int(settings: &JsonValue, key: &str) -> Result<i32, String> {
    let value = required_number(settings, key)?;
    let truncated = value.trunc();
    if !truncated.is_finite() || truncated < f64::from(i32::MIN) || truncated > f64::from(i32::MAX) {
        return Err(format!("`{key}` ({value}) is out of range of int"));
    }
    Ok(truncated as i32)
}

/// Every integer parser of `GlobalParseRegistries.SCALAR_PARSERS_INT`.
fn parse_int_format(value: &JsonValue, depth: usize) -> Result<IntFormat, Problem> {
    if depth > MAX_NESTING {
        return Err("parser nesting too deep".into());
    }
    let (name, settings) = single_member(value)?;
    let delegate = || int_delegate(name, settings, depth);
    Ok(match name {
        "parse_png" | "parse_jpg" => IntFormat::Image,
        "parse_tiff" => IntFormat::Tiff,
        "add" => IntFormat::Add(delegate()?, required_int(settings, "value")?),
        "and" => IntFormat::And(delegate()?, required_int(settings, "mask")?),
        "flip_x" => IntFormat::FlipX(delegate()?),
        "flip_z" => IntFormat::FlipZ(delegate()?),
        "swap_axes" => IntFormat::SwapAxes(delegate()?),
        "grayscale_extract" => IntFormat::GrayscaleExtract(delegate()?),
        "rgb_extract" => IntFormat::RgbExtract(delegate()?),
        "require_opaque" => IntFormat::RequireOpaque(delegate()?),
        other => return Err(format!("unknown integer parser `{other}`").into()),
    })
}

/// Every projection and transform of `GlobalParseRegistries.PROJECTIONS`.
fn parse_projection(value: &JsonValue, depth: usize) -> Result<SharedProjection, Problem> {
    if depth > MAX_NESTING {
        return Err("projection nesting too deep".into());
    }
    let (name, settings) = single_member(value)?;
    let delegate = || -> Result<SharedProjection, Problem> {
        parse_projection(settings.get("delegate").ok_or_else(|| format!("`{name}` requires `delegate`"))?, depth + 1)
    };
    let shared = |projection: SharedProjection| Ok(projection);
    match name {
        "web_mercator" => {
            let zoom = required_number(settings, "zoom")?;
            if zoom.fract() != 0.0 || !(0.0..=30.0).contains(&zoom) {
                return Err(format!("invalid web_mercator zoom {zoom}").into());
            }
            shared(Arc::new(WebMercatorProjection::new(zoom as u32).map_err(Problem::from)?))
        }
        "scale" => shared(Arc::new(ScaleTransform::new(delegate()?, required_number(settings, "x")?, required_number(settings, "y")?).map_err(Problem::from)?)),
        "offset" => shared(Arc::new(OffsetTransform::new(delegate()?, required_number(settings, "dx")?, required_number(settings, "dy")?).map_err(Problem::from)?)),
        "flip_horizontal" => shared(Arc::new(FlipHorizontalTransform::new(delegate()?))),
        "flip_vertical" => shared(Arc::new(FlipVerticalTransform::new(delegate()?))),
        "swap_axes" => shared(Arc::new(SwapAxesTransform::new(delegate()?))),
        "centered_mercator" => shared(Arc::new(CenteredMercatorProjection)),
        "transverse_mercator" => shared(Arc::new(TransverseMercatorProjection)),
        "equirectangular" => shared(Arc::new(EquirectangularProjection)),
        "sinusoidal" => shared(Arc::new(SinusoidalProjection)),
        "equal_earth" => shared(Arc::new(EqualEarthProjection)),
        "dymaxion" => shared(Arc::new(DymaxionProjection::default())),
        "conformal_dymaxion" => shared(Arc::new(ConformalDymaxionProjection::new().map_err(|error| format!("conformal_dymaxion: {error}"))?)),
        "bte_conformal_dymaxion" => shared(Arc::new(BteDymaxionProjection::new().map_err(|error| format!("bte_conformal_dymaxion: {error}"))?)),
        "lambert_azimuthal" => {
            shared(Arc::new(LambertAzimuthalProjection::new(optional_number(settings, "centerX")?, optional_number(settings, "centerY")?).map_err(Problem::from)?))
        }
        "azimuthal_equidistant" => shared(Arc::new(
            AzimuthalEquidistantProjection::new(optional_number(settings, "centerX")?, optional_number(settings, "centerY")?).map_err(Problem::from)?,
        )),
        "stereographic" => shared(Arc::new(
            StereographicProjection::new(optional_number(settings, "centerX")?, optional_number(settings, "centerY")?, optional_number(settings, "radius")?)
                .map_err(Problem::from)?,
        )),
        "clamp" => shared(Arc::new(
            ClampTransform::new(
                delegate()?,
                required_number(settings, "minX")?,
                required_number(settings, "maxX")?,
                required_number(settings, "minY")?,
                required_number(settings, "maxY")?,
            )
            .map_err(Problem::from)?,
        )),
        "rotate" => shared(Arc::new(RotateTransform::new(delegate()?, required_number(settings, "by")?).map_err(Problem::from)?)),
        other => Err(format!("unknown dataset projection `{other}`").into()),
    }
}

fn parse_blend(dataset: &JsonValue) -> Result<BlendMode, String> {
    match dataset.get("blend").and_then(JsonValue::as_str) {
        Some(name) => BlendMode::from_name(name).ok_or_else(|| format!("unknown blend mode `{name}`")),
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
    fn every_integer_parser_chain_loads() {
        let layer = |parse: &str| {
            format!(
                r#"[{{"dataset": {{"urls": ["https://example.invalid/${{x}}/${{z}}.png"], "projection": {{"equirectangular": {{}}}},
                "resolution": 256, "blend": "NEAR", "parse": {parse}}}, "bounds": [{{"minX": -1, "maxX": 1, "minZ": -1, "maxZ": 1}}], "zooms": {{"min": 0, "max": 3}}}}]"#
            )
        };
        let format = |parse: &str| {
            let loaded = load_heights_config(&layer(parse), UnsupportedLayerPolicy::Reject).unwrap_or_else(|error| panic!("{parse}: {error}"));
            assert!(loaded.skipped.is_empty());
            loaded.dataset.layers()[0].dataset.format.clone()
        };
        let png = || Box::new(IntFormat::Image);
        assert_eq!(format(r#"{"from_int": {"delegate": {"parse_png": {}}}}"#), TileFormat::FromInt(png()));
        assert_eq!(format(r#"{"from_int": {"delegate": {"parse_jpg": {}}}}"#), TileFormat::FromInt(png()));
        assert_eq!(
            format(r#"{"multiply": {"value": 0.1, "delegate": {"from_int": {"delegate": {"add": {"value": -100000, "delegate": {"rgb_extract": {"delegate": {"parse_tiff": {}}}}}}}}}}"#),
            TileFormat::Multiply(Box::new(TileFormat::FromInt(Box::new(IntFormat::Add(Box::new(IntFormat::RgbExtract(Box::new(IntFormat::Tiff))), -100000)))), 0.1)
        );
        for (name, expected) in [
            ("flip_x", IntFormat::FlipX(png())),
            ("flip_z", IntFormat::FlipZ(png())),
            ("swap_axes", IntFormat::SwapAxes(png())),
            ("grayscale_extract", IntFormat::GrayscaleExtract(png())),
            ("rgb_extract", IntFormat::RgbExtract(png())),
            ("require_opaque", IntFormat::RequireOpaque(png())),
        ] {
            assert_eq!(format(&format!(r#"{{"from_int": {{"delegate": {{"{name}": {{"delegate": {{"parse_png": {{}}}}}}}}}}}}"#)), TileFormat::FromInt(Box::new(expected)));
        }
        // Jackson's int binding: fractions truncate toward zero, values outside int fail.
        assert_eq!(format(r#"{"from_int": {"delegate": {"and": {"mask": -1.9, "delegate": {"parse_png": {}}}}}}"#), TileFormat::FromInt(Box::new(IntFormat::And(png(), -1))));
        for invalid in [
            r#"{"from_int": {"delegate": {"and": {"mask": 4294967295, "delegate": {"parse_png": {}}}}}}"#,
            r#"{"from_int": {"delegate": {"add": {"delegate": {"parse_png": {}}}}}}"#,
            r#"{"from_int": {"delegate": {"rgb_extract": {}}}}"#,
            r#"{"from_int": {}}"#,
            r#"{"from_int": {"delegate": {"parse_png_terrarium": {}}}}"#,
            r#"{"from_intt": {"delegate": {"parse_png": {}}}}"#,
        ] {
            assert!(matches!(load_heights_config(&layer(invalid), UnsupportedLayerPolicy::Skip), Err(ElevationError::InvalidConfiguration(_))), "{invalid}");
        }
    }

    #[test]
    fn every_registered_projection_loads() {
        let layer = |projection: &str| {
            format!(
                r#"[{{"dataset": {{"urls": ["https://example.invalid/${{x}}/${{z}}.png"], "projection": {projection},
                "resolution": 256, "blend": "NEAR", "parse": {{"parse_tiff": {{}}}}}}, "bounds": [{{"minX": -1, "maxX": 1, "minZ": -1, "maxZ": 1}}], "zooms": {{"min": 0, "max": 3}}}}]"#
            )
        };
        for projection in [
            r#"{"centered_mercator": {}}"#,
            r#"{"transverse_mercator": {}}"#,
            r#"{"equirectangular": {}}"#,
            r#"{"sinusoidal": {}}"#,
            r#"{"equal_earth": {}}"#,
            r#"{"dymaxion": {}}"#,
            r#"{"conformal_dymaxion": {}}"#,
            r#"{"bte_conformal_dymaxion": {}}"#,
            r#"{"lambert_azimuthal": {"centerX": 12.5, "centerY": null}}"#,
            r#"{"azimuthal_equidistant": {}}"#,
            r#"{"stereographic": {"radius": 45}}"#,
            r#"{"clamp": {"delegate": {"equirectangular": {}}, "minX": -10, "maxX": 10, "minY": -5, "maxY": 5}}"#,
            r#"{"rotate": {"delegate": {"equal_earth": {}}, "by": 30}}"#,
        ] {
            let loaded = load_heights_config(&layer(projection), UnsupportedLayerPolicy::Reject);
            assert!(loaded.is_ok(), "{projection}: {:?}", loaded.err());
        }
        for invalid in [r#"{"stereographic": {"radius": 180}}"#, r#"{"lambert_azimuthal": {"centerX": "east"}}"#, r#"{"clamp": {"delegate": {"equirectangular": {}}, "minX": 0}}"#, r#"{"rotate": {"by": 3}}"#] {
            assert!(matches!(load_heights_config(&layer(invalid), UnsupportedLayerPolicy::Skip), Err(ElevationError::InvalidConfiguration(_))), "{invalid}");
        }
    }

    #[test]
    fn default_tree_cover_loads_with_its_parser_chain_and_scaled_projection() {
        let loaded = load_tree_cover_config(DEFAULT_TREE_COVER, UnsupportedLayerPolicy::Reject).unwrap();
        let layers = loaded.dataset.layers();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].dataset.format, TileFormat::Divide(Box::new(TileFormat::Tiff), 100.0));
        assert_eq!(layers[0].dataset.id, "tree-cover-layer-0");
        assert_eq!(layers[0].priority, -100.0);
        assert_eq!((layers[0].bounds.min_z, layers[0].bounds.max_z), (-60.0, 80.0));
    }

    #[test]
    fn nested_parsers_and_projection_transforms_load() {
        let config = r#"[{"dataset": {"urls": "mirror://a/${x}/${z}.tiff",
            "projection": {"flip_vertical": {"delegate": {"offset": {"delegate": {"scale": {"delegate": {"web_mercator": {"zoom": 5}}, "x": 2, "y": 2}}, "dx": 1, "dy": 0}}}},
            "resolution": 256, "blend": "NEAR",
            "parse": {"add": {"delegate": {"multiply": {"delegate": {"swap_axes": {"delegate": {"flip_z": {"delegate": {"flip_x": {"delegate": {"parse_tiff_fp": {}}}}}}}}, "value": 2}}, "value": -1}}},
            "bounds": {"minX": 0, "maxX": 1, "minZ": 0, "maxZ": 1}, "zooms": 0}]"#;
        let loaded = load_heights_config(config, UnsupportedLayerPolicy::Reject).unwrap();
        let format = &loaded.dataset.layers()[0].dataset.format;
        let expected = TileFormat::Add(
            Box::new(TileFormat::Multiply(Box::new(TileFormat::SwapAxes(Box::new(TileFormat::FlipZ(Box::new(TileFormat::FlipX(Box::new(TileFormat::Tiff))))))), 2.0)),
            -1.0,
        );
        assert_eq!(format, &expected);
        let unknown = config.replace("flip_z", "flip_y");
        assert!(matches!(load_heights_config(&unknown, UnsupportedLayerPolicy::Skip), Err(ElevationError::InvalidConfiguration(_))));
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
