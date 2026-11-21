//! `DoubleTiledDataset` / `ConfigurableDoubleTiledDataset`: scalar rasters cut into square tiles.

use crate::blend::BlendMode;
use crate::bounds::{CornerBounds, TilePosition};
use crate::error::ElevationError;
use std::collections::HashMap;
use std::sync::Arc;
use terra_projection::SharedProjection;

/// Hard ceiling on tiles touched by one grid request. A 16x16 chunk needs at most four tiles at
/// any supported zoom; anything far larger indicates a degenerate quadrilateral.
pub const MAX_TILES_PER_REQUEST: u64 = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TileFormat {
    /// `parse_png_terrarium`.
    TerrariumPng,
    /// `parse_tiff`: used by the regional high-resolution datasets; decoding is not implemented.
    Tiff,
}

pub type Tile = Arc<[f64]>;

#[derive(Debug, Clone)]
pub struct TiledScalarDataset {
    /// Stable identifier used in cache keys and diagnostics.
    pub id: String,
    pub url_templates: Vec<String>,
    pub projection: SharedProjection,
    pub blend: BlendMode,
    pub format: TileFormat,
    resolution: u32,
    shift: u32,
}

impl TiledScalarDataset {
    pub fn new(
        id: String,
        url_templates: Vec<String>,
        projection: SharedProjection,
        resolution: u32,
        blend: BlendMode,
        format: TileFormat,
    ) -> Result<Self, ElevationError> {
        if resolution == 0 || !resolution.is_power_of_two() || resolution > 4096 {
            return Err(ElevationError::InvalidConfiguration(format!("given resolution ({resolution}) is not a power of 2!")));
        }
        for template in &url_templates {
            validate_template(template)?;
        }
        Ok(Self { id, url_templates, projection, blend, format, resolution, shift: resolution.trailing_zeros() })
    }

    pub fn resolution(&self) -> u32 {
        self.resolution
    }

    /// The tile URLs for `position`, substituting `${x}` and `${z}`.
    pub fn urls(&self, position: TilePosition) -> Vec<String> {
        self.url_templates
            .iter()
            .map(|template| template.replace("${x}", &position.x.to_string()).replace("${z}", &position.z.to_string()))
            .collect()
    }

    /// Stage one of `getAsync(CornerBoundingBox2d, ...)`: project the geographic quadrilateral into
    /// dataset space and list the tiles the blend will read.
    pub fn plan(&self, geographic: &CornerBounds) -> Result<GridPlan, ElevationError> {
        let projection = self.projection.as_ref();
        let local = geographic.from_geo(projection)?;
        let padded = local.axis_align(projection)?.expand(f64::from(self.blend.size())).validate_projected(projection)?;
        let size = f64::from(self.resolution);
        let count = padded.tile_count(size);
        if count > MAX_TILES_PER_REQUEST {
            return Err(ElevationError::RequestTooLarge { tiles: count });
        }
        Ok(GridPlan { local, tiles: padded.to_tiles(size) })
    }

    /// Stage two: sample a `size_x` by `size_z` grid (`x * size_z + z`) over the planned
    /// quadrilateral. Tiles absent from `tiles`, or present as `None` (HTTP 404), read as `NaN`.
    pub fn sample(&self, plan: &GridPlan, size_x: usize, size_z: usize, tiles: &LoadedTiles) -> Vec<f64> {
        let mut out = Vec::with_capacity(size_x * size_z);
        if size_x == 0 || size_z == 0 {
            return out;
        }
        let shift = self.shift;
        let mask = (self.resolution - 1) as i32;
        let sampler = |x: i32, z: i32| match tiles.get(&TilePosition { x: x >> shift, z: z >> shift }) {
            Some(Some(tile)) => tile[(((z & mask) << shift) | (x & mask)) as usize],
            _ => f64::NAN,
        };
        let step_x = 1.0 / size_x as f64;
        let step_z = 1.0 / size_z as f64;
        // Upstream accumulates the fractions by repeated addition; multiplying would round differently.
        let mut fx = 0.0;
        for _ in 0..size_x {
            let mut fz = 0.0;
            for _ in 0..size_z {
                let (x, z) = plan.local.point(fx, fz);
                out.push(self.blend.get(x, z, sampler));
                fz += step_z;
            }
            fx += step_x;
        }
        out
    }

    pub fn decode(&self, bytes: &[u8]) -> Result<Tile, ElevationError> {
        match self.format {
            TileFormat::TerrariumPng => Ok(crate::terrarium::decode_terrarium_png(bytes, self.resolution)?.into()),
            TileFormat::Tiff => Err(ElevationError::UnsupportedFormat("parse_tiff")),
        }
    }
}

fn validate_template(template: &str) -> Result<(), ElevationError> {
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(ElevationError::InvalidConfiguration(format!("unterminated placeholder in {template}")));
        };
        let key = &after[..end];
        if key != "x" && key != "z" {
            return Err(ElevationError::InvalidConfiguration(format!("unsupported url property \"{key}\" in {template}")));
        }
        rest = &after[end + 1..];
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct GridPlan {
    pub local: CornerBounds,
    pub tiles: Vec<TilePosition>,
}

/// Loaded tiles for one request; `None` records a tile the provider reported as absent.
pub type LoadedTiles = HashMap<TilePosition, Option<Tile>>;
