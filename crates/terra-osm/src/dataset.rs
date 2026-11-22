//! The OpenStreetMap data path of TerraMinusMinus: `ParsingGeoJsonDataset` (line-delimited
//! GeoJSON tiles fetched from the first server that has them), `GeoJsonToVectorDataset`
//! (references resolved and elements mapped), `VectorTiledDataset` (1/64-degree tiles indexed in a
//! BVH) and `OSMBaker` (elements intersecting a chunk, deduplicated and drawn in order).

use crate::bounds::Bounds2d;
use crate::bvh::Bvh;
use crate::cache::{CacheStatistics, LoadingCache};
use crate::draw::DrawTarget;
use crate::geojson::{GeoJsonObject, GeoPoint, Geometry, Tags, parse_geojson_lines};
use crate::geometry::{Point, VectorGeometry};
use crate::mapper::{MapperError, OsmMapper};
use std::fmt;
use std::sync::{Arc, LazyLock};
use terra_elevation::numeric::{ceil_i, floor_i, java_max, java_min};
use terra_elevation::{FetchOutcome, TileFetcher};
use terra_projection::{GeoCoordinate, GeographicProjection, ProjectedCoordinate, SharedProjection};

/// `TerraConfig.openstreetmap.servers`.
pub const DEFAULT_OSM_SERVERS: [&str; 1] = ["https://cloud.daporkchop.net/gis/osm/0/"];
/// `VectorTiledDataset`: 1/64 of a degree in an equirectangular grid.
pub const TILES_PER_DEGREE: f64 = 64.0;
/// One chunk needs at least 2x2 tiles; projection folds can make the box span far more. Beyond
/// this the request is refused instead of fetching a whole region of the planet for one chunk.
pub const MAX_TILES_PER_CHUNK: usize = 1024;
const MAX_REFERENCE_DEPTH: usize = 16;
const MAX_KEY_LENGTH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsmError {
    /// Every server answered 404; upstream fails the chunk the same way.
    Missing { key: String },
    Unavailable { key: String, reasons: String },
    Malformed { key: String, reason: String },
    InvalidReference { key: String, location: String },
    ReferenceCycle { location: String },
    TooManyTiles { chunk: (i32, i32), tiles: usize },
}

impl fmt::Display for OsmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { key } => write!(f, "OpenStreetMap resource {key} is missing on every server"),
            Self::Unavailable { key, reasons } => write!(f, "OpenStreetMap resource {key} could not be fetched: {reasons}"),
            Self::Malformed { key, reason } => write!(f, "OpenStreetMap resource {key} is malformed: {reason}"),
            Self::InvalidReference { key, location } => write!(f, "OpenStreetMap resource {key} references the invalid location {location:?}"),
            Self::ReferenceCycle { location } => write!(f, "OpenStreetMap references loop through {location} (or nest deeper than {MAX_REFERENCE_DEPTH})"),
            Self::TooManyTiles { chunk, tiles } => write!(f, "chunk {chunk:?} would need {tiles} OpenStreetMap tiles (limit {MAX_TILES_PER_CHUNK})"),
        }
    }
}

impl std::error::Error for OsmError {}

#[derive(Debug, Clone, PartialEq)]
pub struct OsmSettings {
    /// URL prefixes tried in order; a resource key is appended to each.
    pub servers: Vec<String>,
    /// Parsed tiles kept in memory (each holds the elements of 1/64 x 1/64 degrees).
    pub tile_cache_entries: usize,
    /// Resolved resources kept in memory, tiles and the shared resources they reference.
    pub resource_cache_entries: usize,
}

impl Default for OsmSettings {
    fn default() -> Self {
        Self { servers: DEFAULT_OSM_SERVERS.iter().map(|server| (*server).to_owned()).collect(), tile_cache_entries: 256, resource_cache_entries: 1024 }
    }
}

/// Elements intersecting a chunk, or `OutsideProjection` when its surroundings cannot be
/// converted to geographic coordinates (`OSMBaker` then marks the whole chunk as ocean).
#[derive(Debug, Clone)]
pub enum ChunkElements {
    OutsideProjection,
    Elements(Vec<Arc<VectorGeometry>>),
}

type Elements = Arc<[Arc<VectorGeometry>]>;
type TileIndex = Arc<Bvh<Arc<VectorGeometry>>>;

pub struct OsmDataset {
    mapper: Arc<OsmMapper>,
    projection: SharedProjection,
    fetcher: Arc<dyn TileFetcher>,
    servers: Vec<String>,
    resources: LoadingCache<String, Elements, OsmError>,
    tiles: LoadingCache<(i32, i32), TileIndex, OsmError>,
}

static NO_TAGS: LazyLock<Tags> = LazyLock::new(Tags::new);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OsmStatistics {
    pub tiles: CacheStatistics,
    pub resources: CacheStatistics,
}

impl OsmDataset {
    pub fn new(mapper: Arc<OsmMapper>, projection: SharedProjection, fetcher: Arc<dyn TileFetcher>, settings: &OsmSettings) -> Self {
        Self {
            mapper,
            projection,
            fetcher,
            servers: settings.servers.clone(),
            resources: LoadingCache::new(settings.resource_cache_entries),
            tiles: LoadingCache::new(settings.tile_cache_entries),
        }
    }

    pub fn mapper(&self) -> &OsmMapper {
        &self.mapper
    }

    pub fn statistics(&self) -> OsmStatistics {
        OsmStatistics { tiles: self.tiles.statistics(), resources: self.resources.statistics() }
    }

    /// `OSMBaker.requestData` and the element collection of `OSMBaker.bake`.
    pub fn chunk_elements(&self, chunk_x: i32, chunk_z: i32) -> Result<ChunkElements, OsmError> {
        let base_x = f64::from(chunk_x.wrapping_shl(4));
        let base_z = f64::from(chunk_z.wrapping_shl(4));
        let Some(corners) = corner_box_to_geo(self.projection.as_ref(), base_x - 16.0, base_z - 16.0, base_x + 32.0, base_z + 32.0) else {
            return Ok(ChunkElements::OutsideProjection);
        };
        let Some(range) = tile_range(&corners) else {
            return Ok(ChunkElements::OutsideProjection);
        };
        if range.count() > MAX_TILES_PER_CHUNK as u64 {
            return Err(OsmError::TooManyTiles { chunk: (chunk_x, chunk_z), tiles: usize::try_from(range.count()).unwrap_or(usize::MAX) });
        }
        let chunk_bounds = Bounds2d::of(base_x, base_x + 16.0, base_z, base_z + 16.0);
        let mut ordered: Vec<Arc<VectorGeometry>> = Vec::new();
        for tile in range.tiles() {
            let index = self.tile(tile)?;
            index.for_each_intersecting(&chunk_bounds, &mut |element| {
                if let Err(position) = ordered.binary_search_by(|existing| existing.draw_order(element)) {
                    ordered.insert(position, Arc::clone(element));
                }
            });
        }
        Ok(ChunkElements::Elements(ordered))
    }

    /// The elements of one 1/64-degree tile in the order upstream produces them: referenced
    /// resources first, then the tile's own objects.
    pub fn tile_elements(&self, tile_x: i32, tile_z: i32) -> Result<Vec<Arc<VectorGeometry>>, OsmError> {
        Ok(self.resolve(&format!("tile/{tile_x}/{tile_z}.json"), &mut Vec::new())?.to_vec())
    }

    fn tile(&self, (x, z): (i32, i32)) -> Result<TileIndex, OsmError> {
        self.tiles.get_or_load((x, z), || {
            let elements = self.resolve(&format!("tile/{x}/{z}.json"), &mut Vec::new())?;
            Ok(Arc::new(Bvh::of(elements.to_vec())))
        })
    }

    /// `AbstractReferenceResolvingGeoJsonDataset.load`: referenced resources first, in order, then
    /// the elements of the resource's own objects. Shared resources are cached but not
    /// deduplicated in flight, so two tiles referencing each other cannot deadlock.
    fn resolve(&self, key: &str, chain: &mut Vec<String>) -> Result<Elements, OsmError> {
        if let Some(elements) = self.resources.get(&key.to_owned()) {
            return Ok(elements);
        }
        if chain.iter().any(|visited| visited == key) || chain.len() >= MAX_REFERENCE_DEPTH {
            return Err(OsmError::ReferenceCycle { location: key.to_owned() });
        }
        let objects = self.fetch_objects(key)?;
        let mut elements = Vec::new();
        let mut local = Vec::new();
        chain.push(key.to_owned());
        for object in &objects {
            match object {
                GeoJsonObject::Reference(location) => {
                    if !is_valid_key(location) {
                        chain.pop();
                        return Err(OsmError::InvalidReference { key: key.to_owned(), location: location.clone() });
                    }
                    match self.resolve(location, chain) {
                        Ok(resolved) => elements.extend(resolved.iter().cloned()),
                        Err(error) => {
                            chain.pop();
                            return Err(error);
                        }
                    }
                }
                other => local.push(other),
            }
        }
        chain.pop();
        for object in local {
            self.convert_object(None, &NO_TAGS, object, &mut elements).map_err(|error| OsmError::Malformed { key: key.to_owned(), reason: error.to_string() })?;
        }
        let elements: Elements = elements.into();
        self.resources.insert(key.to_owned(), Arc::clone(&elements));
        Ok(elements)
    }

    /// `Http.getFirst` + `ParsingGeoJsonDataset.decode`: a server whose answer fails to parse
    /// counts as failed and the next one is tried.
    fn fetch_objects(&self, key: &str) -> Result<Vec<GeoJsonObject>, OsmError> {
        let mut missing = false;
        let mut reasons = Vec::new();
        for server in &self.servers {
            let url = format!("{server}{key}");
            match self.fetcher.fetch(&url) {
                Ok(FetchOutcome::Found(bytes)) => match parse_geojson_lines(&bytes) {
                    Ok(objects) => return Ok(objects),
                    Err(error) => reasons.push(format!("{url}: {error}")),
                },
                Ok(FetchOutcome::Missing) => missing = true,
                Err(error) => reasons.push(format!("{url}: {error}")),
            }
        }
        match (missing, reasons.is_empty()) {
            (true, _) => Err(OsmError::Missing { key: key.to_owned() }),
            (false, true) => Err(OsmError::Unavailable { key: key.to_owned(), reasons: "no servers configured".into() }),
            (false, false) => Err(OsmError::Unavailable { key: key.to_owned(), reasons: reasons.join("; ") }),
        }
    }

    /// `GeoJsonToVectorDataset.convertToElements`. Every `Iterable` is walked, which includes
    /// `MultiLineString` and `MultiPolygon`: their parts are mapped one by one under the same id.
    fn convert_object(&self, id: Option<&str>, tags: &Tags, object: &GeoJsonObject, out: &mut Vec<Arc<VectorGeometry>>) -> Result<(), MapperError> {
        match object {
            GeoJsonObject::Reference(_) => Ok(()),
            GeoJsonObject::FeatureCollection(features) => {
                for feature in features {
                    self.convert_feature(id, tags, feature, out)?;
                }
                Ok(())
            }
            GeoJsonObject::Feature(feature) => self.convert_feature(id, tags, feature, out),
            GeoJsonObject::Geometry(geometry) => self.convert_geometry(id, tags, geometry, out),
        }
    }

    fn convert_feature(&self, id: Option<&str>, tags: &Tags, feature: &crate::geojson::Feature, out: &mut Vec<Arc<VectorGeometry>>) -> Result<(), MapperError> {
        let id = feature.id.as_deref().or(id);
        let tags = feature.properties.as_deref().unwrap_or(tags);
        self.convert_geometry(id, tags, &feature.geometry, out)
    }

    fn convert_geometry(&self, id: Option<&str>, tags: &Tags, geometry: &Geometry, out: &mut Vec<Arc<VectorGeometry>>) -> Result<(), MapperError> {
        match geometry {
            Geometry::GeometryCollection(children) => children.iter().try_for_each(|child| self.convert_geometry(id, tags, child, out)),
            Geometry::MultiLineString(lines) => lines.iter().try_for_each(|line| self.map(id, tags, &Geometry::LineString(line.clone()), out)),
            Geometry::MultiPolygon(polygons) => polygons.iter().try_for_each(|polygon| self.map(id, tags, &Geometry::Polygon(polygon.clone()), out)),
            Geometry::Point(_) | Geometry::MultiPoint(_) => Ok(()),
            Geometry::LineString(_) | Geometry::Polygon(_) => self.map(id, tags, geometry, out),
        }
    }

    fn map(&self, id: Option<&str>, tags: &Tags, geometry: &Geometry, out: &mut Vec<Arc<VectorGeometry>>) -> Result<(), MapperError> {
        let projection = self.projection.as_ref();
        let project = |point: GeoPoint| {
            projection.from_geo(GeoCoordinate { longitude: point.lon, latitude: point.lat }).ok().map(|projected| Point { x: projected.x, z: projected.y })
        };
        if let Some(elements) = self.mapper.apply(id, tags, geometry, &project)? {
            out.extend(elements);
        }
        Ok(())
    }
}

/// `OSMBaker.bake` for a chunk whose elements were found: draws them in layer and id order.
pub fn apply_elements(elements: &[Arc<VectorGeometry>], target: &mut dyn DrawTarget, chunk_x: i32, chunk_z: i32) {
    let base_x = f64::from(chunk_x.wrapping_shl(4));
    let base_z = f64::from(chunk_z.wrapping_shl(4));
    let chunk_bounds = Bounds2d::of(base_x, base_x + 16.0, base_z, base_z + 16.0);
    for element in elements {
        element.apply(target, chunk_x, chunk_z, &chunk_bounds);
    }
}

/// Resource keys come from remote data; only plain relative paths are followed.
fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LENGTH
        && !key.starts_with('/')
        && key.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b'/'))
        && key.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

/// `Bounds2d.toCornerBB(projection, false).toGeo()` including both validations: every local
/// corner must convert to geographic coordinates and every result back. Corner order: 00, 01, 10, 11.
fn corner_box_to_geo(projection: &dyn GeographicProjection, min_x: f64, min_z: f64, max_x: f64, max_z: f64) -> Option<[GeoCoordinate; 4]> {
    let size_x = max_x - min_x;
    let size_z = max_z - min_z;
    let local = [(min_x, min_z), (min_x, min_z + size_z), (min_x + size_x, min_z), (min_x + size_x, min_z + size_z)];
    let mut geo = [GeoCoordinate { longitude: 0.0, latitude: 0.0 }; 4];
    for (slot, (x, z)) in geo.iter_mut().zip(local) {
        *slot = projection.to_geo(ProjectedCoordinate::new(x, z)).ok()?;
    }
    for corner in &geo {
        projection.from_geo(*corner).ok()?;
    }
    Some(geo)
}

/// Inclusive tile range of `Bounds2d.toTiles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TileRange {
    min_x: i32,
    max_x: i32,
    min_z: i32,
    max_z: i32,
}

impl TileRange {
    fn count(self) -> u64 {
        let span = |min: i32, max: i32| u64::try_from(i64::from(max) - i64::from(min) + 1).unwrap_or(0);
        span(self.min_x, self.max_x) * span(self.min_z, self.max_z)
    }

    /// X-major, like `toTiles`.
    fn tiles(self) -> impl Iterator<Item = (i32, i32)> {
        (self.min_x..=self.max_x).flat_map(move |x| (self.min_z..=self.max_z).map(move |z| (x, z)))
    }
}

/// `CornerBoundingBox2d.fromGeo(equirectangular).axisAlign().toTiles(1 / 64)`; `None` when a
/// corner is outside the equirectangular range (upstream throws `OutOfProjectionBoundsException`).
fn tile_range(corners: &[GeoCoordinate; 4]) -> Option<TileRange> {
    let in_range = |corner: &GeoCoordinate| !(corner.longitude.abs() > 180.0 || corner.latitude.abs() > 90.0);
    if !corners.iter().all(in_range) {
        return None;
    }
    let fold = |pick: fn(&GeoCoordinate) -> f64, combine: fn(f64, f64) -> f64| corners.iter().map(pick).reduce(combine).unwrap_or(f64::NAN);
    Some(TileRange {
        min_x: floor_i(fold(|c| c.longitude, java_min) * TILES_PER_DEGREE),
        max_x: ceil_i(fold(|c| c.longitude, java_max) * TILES_PER_DEGREE),
        min_z: floor_i(fold(|c| c.latitude, java_min) * TILES_PER_DEGREE),
        max_z: ceil_i(fold(|c| c.latitude, java_max) * TILES_PER_DEGREE),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_from_remote_data_must_be_plain_relative_paths() {
        for valid in ["tile/1/-2.json", "multi/coastline/5.json"] {
            assert!(is_valid_key(valid), "{valid}");
        }
        for invalid in ["", "/etc/passwd", "../x.json", "tile/../../x", "tile//x", "tile/x?y", "https://evil/x", "tile/./x"] {
            assert!(!is_valid_key(invalid), "{invalid}");
        }
    }

    #[test]
    fn tile_ranges_include_the_ceiling_tile() {
        let corner = |longitude, latitude| GeoCoordinate { longitude, latitude };
        let range = tile_range(&[corner(1.0 / 128.0, -0.001), corner(1.0 / 128.0, 0.001), corner(0.01, -0.001), corner(0.01, 0.001)]).unwrap();
        assert_eq!(range.count(), 6);
        assert_eq!(range.tiles().collect::<Vec<_>>(), vec![(0, -1), (0, 0), (0, 1), (1, -1), (1, 0), (1, 1)]);
        assert!(tile_range(&[corner(181.0, 0.0), corner(0.0, 0.0), corner(0.0, 0.0), corner(0.0, 0.0)]).is_none());
    }
}
