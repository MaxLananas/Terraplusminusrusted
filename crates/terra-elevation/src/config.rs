//! Loads elevation layer configs in TerraMinusMinus' `heights.json5` schema.

use crate::blend::BlendMode;
use crate::bounds::AxisBounds;
use crate::condition::DoubleCondition;
use crate::dataset::{TileFormat, TiledScalarDataset};
use crate::error::ElevationError;
use crate::int_format::IntFormat;
use crate::layered::{LayeredScalarDataset, OptionalLayer, ScalarLayer};
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

/// Upstream's complete default `heights.json5` (TerraMinusMinus a1432a9, verbatim): the AWS
/// Terrain Tiles world layers plus every regional DEM upstream serves (France's IGN RGE ALTI 1 m,
/// Spain, Germany, the United States, ...).
pub const DEFAULT_WORLD_HEIGHTS: &str = include_str!("../resources/heights_world.json5");

/// IGN LiDAR HD terrain model layers for France (this port's addition, see
/// `resources/heights_ign_lidar_hd.json5`), loaded above the heights config when
/// `elevation.ign_lidar_hd` is on.
pub const IGN_LIDAR_HD_HEIGHTS: &str = include_str!("../resources/heights_ign_lidar_hd.json5");

/// The upstream default `tree_cover.json5` (treecover2000 GeoTIFFs), verbatim.
pub const DEFAULT_TREE_COVER: &str = include_str!("../resources/tree_cover.json5");

pub fn load_heights_config(text: &str, policy: UnsupportedLayerPolicy) -> Result<LoadedLayers, ElevationError> {
    load_scalar_config(text, policy, "heights", "layer")
}

/// A second heights config merged over a first: its tile cache keys use `id_prefix` so its
/// layers never share cached tiles with the first config's `layer-<n>` datasets.
pub fn load_additional_heights_config(text: &str, policy: UnsupportedLayerPolicy, id_prefix: &str) -> Result<LoadedLayers, ElevationError> {
    if id_prefix.is_empty() || id_prefix == "layer" || id_prefix == "tree-cover-layer" || !id_prefix.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(invalid(format!("invalid layer id prefix `{id_prefix}`")));
    }
    load_scalar_config(text, policy, "heights", id_prefix)
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
        let optional = match entry.get("optional") {
            None | Some(JsonValue::Bool(false)) => None,
            Some(JsonValue::Bool(true)) => Some(Arc::new(OptionalLayer::default())),
            Some(_) => return Err(context("`optional` is not a boolean".into())),
        };
        let id = format!("{id_prefix}-{index}");
        let tiled = match web_mercator_zoom(dataset.get("projection")) {
            Some(zoom) => TiledScalarDataset::new_web_mercator(id, urls, projection, zoom, resolution as u32, blend, format),
            None => TiledScalarDataset::new(id, urls, projection, resolution as u32, blend, format),
        }
        .map_err(|error| match error {
            ElevationError::InvalidConfiguration(message) => context(message),
            other => other,
        })?;
        let tiled = Arc::new(tiled);
        for bound in bounds.as_array_lenient() {
            layers.push(ScalarLayer { dataset: Arc::clone(&tiled), bounds: parse_bounds(bound).map_err(context)?, priority, condition: condition.clone(), optional: optional.clone() });
        }
    }
    if layers.is_empty() {
        return Err(invalid(format!("{kind} config defines no usable layers")));
    }
    Ok(LoadedLayers { dataset: LayeredScalarDataset::new(layers), skipped })
}

/// The zoom of a projection that is exactly `{"web_mercator": {"zoom": z}}` (no transform
/// around it): only then is a tile's EPSG:3857 extent known in closed form.
fn web_mercator_zoom(projection: Option<&JsonValue>) -> Option<u32> {
    match single_member(projection?) {
        Ok(("web_mercator", settings)) => settings.get("zoom").and_then(JsonValue::as_f64).filter(|zoom| zoom.fract() == 0.0 && (0.0..=22.0).contains(zoom)).map(|zoom| zoom as u32),
        _ => None,
    }
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
        // This port's extension: values failing `condition` become NaN before blending.
        "filter" => {
            let condition = parse_condition(settings.get("condition").ok_or("`filter` requires `condition`")?, 0)?;
            TileFormat::Filter(delegate()?, condition)
        }
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
        // One layer per bounds entry: upstream's 1966 datasets flatten to more layers.
        assert!(layers.len() > 1966, "{}", layers.len());
        assert_eq!(layers[1].condition, Some(DoubleCondition::LessThan(1.0)));
        assert_eq!(layers[1].priority, 1.0);
        // France: IGN RGE ALTI 1 m mirrored by cloud.daporkchop.net, above the AWS layers.
        let paris = AxisBounds { min_x: 2.35, max_x: 2.36, min_z: 48.85, max_z: 48.86 };
        let over_paris = loaded.dataset.intersecting(&paris);
        assert_eq!(over_paris[0].dataset.url_templates, ["https://cloud.daporkchop.net/gis/dem/earth/fr/17/${x}/${z}.tiff"]);
        assert_eq!(over_paris[0].priority, 100.0);
    }

    #[test]
    fn ign_lidar_hd_layers_load_above_france() {
        let loaded = load_additional_heights_config(IGN_LIDAR_HD_HEIGHTS, UnsupportedLayerPolicy::Reject, "ign-lidar-hd").unwrap();
        assert!(loaded.skipped.is_empty());
        let layers = loaded.dataset.layers();
        assert_eq!(layers.len(), 7);
        for layer in layers {
            assert!(layer.dataset.id.starts_with("ign-lidar-hd-"));
            assert_eq!(layer.priority, 150.0);
            assert_eq!(layer.dataset.blend, crate::blend::BlendMode::Linear);
            assert_eq!(layer.dataset.format, TileFormat::Filter(Box::new(TileFormat::Tiff), DoubleCondition::GreaterThan(-1000.0)));
            assert!(layer.dataset.url_templates[0].starts_with("https://data.geopf.fr/wms-r?SERVICE=WMS&VERSION=1.3.0&REQUEST=GetMap&"));
            assert!(layer.optional.is_some());
        }
        // The Eiffel Tower's zoom-17 tile, as an EPSG:3857 WMS bbox.
        let url = &layers[0].dataset.urls(crate::bounds::TilePosition { x: 66371, z: 45091 })[0];
        assert!(url.contains("LAMB93"));
        assert!(url.contains("&BBOX=255299.0773,6250715.0222,255604.8254,6251020.7703&"), "{url}");
        assert!(load_additional_heights_config(IGN_LIDAR_HD_HEIGHTS, UnsupportedLayerPolicy::Reject, "layer").is_err());
        assert!(load_additional_heights_config(IGN_LIDAR_HD_HEIGHTS, UnsupportedLayerPolicy::Reject, "a/b").is_err());
    }

    /// A WMS server as GDAL/MapServer behave: pixel `(i, j)` holds the value at the centre of its
    /// cell in the requested `BBOX`. Heights are a plane in EPSG:3857 metres, and everything east
    /// of `cut` is "not surveyed yet" (-9999). Other URLs get a flat 7 m (the "RGE ALTI" layer).
    struct FakeWms {
        origin: (f64, f64),
        cut: f64,
    }

    impl FakeWms {
        fn height(&self, easting: f64, northing: f64) -> f64 {
            (easting - self.origin.0) * 0.01 + (northing - self.origin.1) * 0.02
        }
    }

    impl crate::provider::TileFetcher for FakeWms {
        fn fetch(&self, url: &str) -> Result<crate::provider::FetchOutcome, ElevationError> {
            let size = 256usize;
            let values: Vec<f32> = match url.split_once("&BBOX=") {
                Some((_, rest)) => {
                    assert!(url.contains("&WIDTH=256&HEIGHT=256") && url.contains("&CRS=EPSG:3857&"), "{url}");
                    let bbox: Vec<f64> = rest.split('&').next().unwrap().split(',').map(|value| value.parse().unwrap()).collect();
                    let (pixel_x, pixel_y) = ((bbox[2] - bbox[0]) / size as f64, (bbox[3] - bbox[1]) / size as f64);
                    (0..size * size)
                        .map(|index| {
                            let easting = bbox[0] + ((index % size) as f64 + 0.5) * pixel_x;
                            let northing = bbox[3] - ((index / size) as f64 + 0.5) * pixel_y;
                            if easting > self.cut { -9999.0 } else { self.height(easting, northing) as f32 }
                        })
                        .collect()
                }
                None => vec![7.0; size * size],
            };
            Ok(crate::provider::FetchOutcome::Found(crate::dataset::tests::float_tiff(size as u32, &values, Some("-9999"))))
        }
    }

    #[test]
    fn ign_wms_pixels_land_where_they_are_sampled_and_gaps_fall_through() {
        use terra_projection::GeographicProjection;
        let half_world = std::f64::consts::PI * 6_378_137.0;
        let to_3857 = |lon: f64, lat: f64| (lon.to_radians() * 6_378_137.0, 6_378_137.0 * (std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0).tan().ln());
        let ign = load_additional_heights_config(IGN_LIDAR_HD_HEIGHTS, UnsupportedLayerPolicy::Reject, "ign-lidar-hd").unwrap();
        let fallback = load_heights_config(
            r#"[{"dataset": {"urls": ["https://example.invalid/rge/${x}/${z}.tiff"], "projection": {"web_mercator": {"zoom": 17}},
            "resolution": 256, "blend": "NEAR", "parse": {"parse_tiff": {}}}, "bounds": [{"minX": -5.6, "maxX": 8.5, "minZ": 42, "maxZ": 51.1}],
            "zooms": {"min": 0, "max": 1}, "priority": 100}]"#,
            UnsupportedLayerPolicy::Reject,
        )
        .unwrap();
        let mut layers = ign.dataset.layers().to_vec();
        layers.extend(fallback.dataset.layers().iter().cloned());
        let dataset = LayeredScalarDataset::new(layers);
        let (cut, _) = to_3857(2.2947, 48.8584);
        let provider = crate::provider::CachingTileProvider::new(FakeWms { origin: to_3857(2.2945, 48.8584), cut }, 64);
        // Around the Eiffel Tower, spanning tile edges and the survey gap.
        let geo = AxisBounds::of(2.2930, 2.2960, 48.8575, 48.8593);
        let mercator = terra_projection::WebMercatorProjection::new(17).unwrap();
        let area = crate::bounds::CornerBounds::from_axis(&geo, &mercator, crate::bounds::CoordinateSpace::Geographic).unwrap();
        let (size_x, size_z) = (24, 24);
        let values = dataset.sample_grid(&area, size_x, size_z, &provider).unwrap().unwrap();
        let plan = dataset.layers()[0].dataset.plan(&area).unwrap();
        assert!(plan.tiles.len() > 1, "the area must cross tile edges: {:?}", plan.tiles);
        let metres_per_pixel = 2.0 * half_world / f64::from(256u32 << 17);
        let (mut checked_lidar, mut checked_fallback, mut fx) = (0, 0, 0.0);
        for x in 0..size_x {
            let mut fz = 0.0;
            for z in 0..size_z {
                let (pixel_x, pixel_z) = plan.local.point(fx, fz);
                let (easting, northing) = (pixel_x * metres_per_pixel - half_world, half_world - pixel_z * metres_per_pixel);
                let value = values[x * size_z + z];
                if easting < cut - 3.0 * metres_per_pixel {
                    // float32 heights of a few metres: ~1e-6 m of rounding.
                    let expected = FakeWms { origin: to_3857(2.2945, 48.8584), cut }.height(easting, northing);
                    assert!((value - expected).abs() < 1e-4, "({x}, {z}) at {easting:.3},{northing:.3}: {value} != {expected}");
                    checked_lidar += 1;
                } else if easting > cut + 3.0 * metres_per_pixel {
                    assert_eq!(value, 7.0, "({x}, {z}): the gap must come from the layer below");
                    checked_fallback += 1;
                }
                fz += 1.0 / size_z as f64;
            }
            fx += 1.0 / size_x as f64;
        }
        assert!(checked_lidar > 100 && checked_fallback > 100, "{checked_lidar} {checked_fallback}");
        // The projected sample points really are the geographic ones.
        let (pixel_x, pixel_z) = plan.local.point(0.0, 0.0);
        let corner = mercator.to_geo(terra_projection::ProjectedCoordinate::new(pixel_x, pixel_z)).unwrap();
        assert!((corner.longitude - 2.2930).abs() < 1e-9 && (corner.latitude - 48.8575).abs() < 1e-9, "{corner:?}");
    }

    #[test]
    fn bbox_3857_needs_plain_web_mercator() {
        let layer = |projection: &str| {
            format!(
                r#"[{{"dataset": {{"urls": ["https://example.invalid/wms?BBOX=${{bbox_3857}}"], "projection": {projection},
                "resolution": 256, "blend": "NEAR", "parse": {{"parse_tiff": {{}}}}}}, "bounds": [{{"minX": -1, "maxX": 1, "minZ": -1, "maxZ": 1}}], "zooms": {{"min": 0, "max": 3}}}}]"#
            )
        };
        let loaded = load_heights_config(&layer(r#"{"web_mercator": {"zoom": 0}}"#), UnsupportedLayerPolicy::Reject).unwrap();
        let world = &loaded.dataset.layers()[0].dataset;
        assert_eq!(world.urls(crate::bounds::TilePosition { x: 0, z: 0 }), ["https://example.invalid/wms?BBOX=-20037508.3428,-20037508.3428,20037508.3428,20037508.3428"]);
        for projection in [r#"{"equirectangular": {}}"#, r#"{"flip_vertical": {"delegate": {"web_mercator": {"zoom": 3}}}}"#] {
            let error = load_heights_config(&layer(projection), UnsupportedLayerPolicy::Reject).unwrap_err().to_string();
            assert!(error.contains("bbox_3857"), "{projection}: {error}");
        }
    }

    #[test]
    fn filter_turns_rejected_values_into_nan() {
        let layer = |parse: &str| {
            format!(
                r#"[{{"dataset": {{"urls": ["https://example.invalid/${{x}}/${{z}}.tiff"], "projection": {{"equirectangular": {{}}}},
                "resolution": 256, "blend": "NEAR", "parse": {parse}}}, "bounds": [{{"minX": -1, "maxX": 1, "minZ": -1, "maxZ": 1}}], "zooms": {{"min": 0, "max": 3}}}}]"#
            )
        };
        let loaded = load_heights_config(&layer(r#"{"filter": {"condition": {"greater_than": -1000}, "delegate": {"parse_tiff": {}}}}"#), UnsupportedLayerPolicy::Reject).unwrap();
        let format = &loaded.dataset.layers()[0].dataset.format;
        let mut tiff = crate::dataset::tests::float_tiff(2, &[-9999.0, 12.5, -99999.0, -10.0], None);
        let values = format.parse(&tiff, 2).unwrap();
        assert!(values[0].is_nan() && values[2].is_nan());
        assert_eq!((values[1], values[3]), (12.5, -10.0));
        tiff.truncate(8);
        assert!(format.parse(&tiff, 2).is_err());
        for broken in [r#"{"filter": {"delegate": {"parse_tiff": {}}}}"#, r#"{"filter": {"condition": {"greater_than": 0}}}"#] {
            assert!(load_heights_config(&layer(broken), UnsupportedLayerPolicy::Reject).is_err(), "{broken}");
        }
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
