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

/// A `DoubleScalarParser` chain: a decoder, optionally wrapped by arithmetic and axis operators.
#[derive(Debug, Clone, PartialEq)]
pub enum TileFormat {
    /// `parse_png_terrarium`.
    TerrariumPng,
    /// `parse_tiff` (and the deprecated `parse_tiff_auto`, `parse_tiff_fp`, `parse_tiff_int`):
    /// single-channel TIFF, values widened from `f32`, the GDAL no-data value becoming `NaN`.
    Tiff,
    /// `add`.
    Add(Box<TileFormat>, f64),
    /// `multiply`.
    Multiply(Box<TileFormat>, f64),
    /// `divide`: multiplies by `1.0 / value`, as upstream.
    Divide(Box<TileFormat>, f64),
    /// `flip_x`.
    FlipX(Box<TileFormat>),
    /// `flip_z`.
    FlipZ(Box<TileFormat>),
    /// `swap_axes`.
    SwapAxes(Box<TileFormat>),
}

impl TileFormat {
    /// `DoubleScalarParser.parse`: a square `resolution` raster, `z * resolution + x`.
    pub fn parse(&self, bytes: &[u8], resolution: u32) -> Result<Vec<f64>, ElevationError> {
        let size = resolution as usize;
        Ok(match self {
            Self::TerrariumPng => crate::terrarium::decode_terrarium_png(bytes, resolution)?,
            Self::Tiff => decode_tiff(bytes, resolution)?,
            Self::Add(inner, value) => inner.parse(bytes, resolution)?.into_iter().map(|sample| sample + value).collect(),
            Self::Multiply(inner, value) => inner.parse(bytes, resolution)?.into_iter().map(|sample| sample * value).collect(),
            Self::Divide(inner, value) => {
                let factor = 1.0 / value;
                inner.parse(bytes, resolution)?.into_iter().map(|sample| sample * factor).collect()
            }
            Self::FlipX(inner) => {
                let mut values = inner.parse(bytes, resolution)?;
                values.chunks_exact_mut(size).for_each(<[f64]>::reverse);
                values
            }
            Self::FlipZ(inner) => {
                let values = inner.parse(bytes, resolution)?;
                values.chunks_exact(size).rev().flatten().copied().collect()
            }
            Self::SwapAxes(inner) => {
                let mut values = inner.parse(bytes, resolution)?;
                for i in 1..size {
                    for j in 0..i {
                        values.swap(i * size + j, j * size + i);
                    }
                }
                values
            }
        })
    }
}

/// `ParseTiffDSP.parse` and `postProcess`.
fn decode_tiff(bytes: &[u8], resolution: u32) -> Result<Vec<f64>, ElevationError> {
    let pixels = u64::from(resolution) * u64::from(resolution);
    let raster = terra_codec::decode_tiff_scalar(bytes, pixels)?;
    if raster.width != resolution || raster.height != resolution {
        return Err(ElevationError::TileDecode(terra_codec::DecodeError::UnsupportedTiff(format!(
            "invalid image resolution: {}x{} (expected: {resolution}x{resolution})",
            raster.width, raster.height
        ))));
    }
    let mut values: Vec<f64> = raster.data.iter().map(|&sample| f64::from(sample)).collect();
    if let Some(text) = &raster.gdal_nodata {
        // `Double.parseDouble(nodataField.getStringValue())`, compared against the widened floats.
        let nodata = terra_codec::parse_java_double(text)
            .ok_or(ElevationError::TileDecode(terra_codec::DecodeError::InvalidTiff("GDAL_NODATA is not a number")))?;
        for value in &mut values {
            if *value == nodata {
                *value = f64::NAN;
            }
        }
    }
    Ok(values)
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
        Ok(self.format.parse(bytes, self.resolution)?.into())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal little-endian, uncompressed, single-strip float32 TIFF. The no-data text must
    /// be longer than three characters (shorter ASCII values belong inline in the entry).
    fn float_tiff(size: u32, values: &[f32], nodata: Option<&str>) -> Vec<u8> {
        let pixel_offset = 8u32;
        let pixel_bytes = size * size * 4;
        let mut nodata_bytes = nodata.map(|text| format!("{text}\0").into_bytes()).unwrap_or_default();
        if nodata_bytes.len() % 2 == 1 {
            nodata_bytes.push(0);
        }
        let nodata_offset = pixel_offset + pixel_bytes;
        let ifd_offset = nodata_offset + nodata_bytes.len() as u32;
        let mut out = b"II*\0".to_vec();
        out.extend_from_slice(&ifd_offset.to_le_bytes());
        for value in values {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&nodata_bytes);
        let mut entries: Vec<(u16, u16, u32, u32)> = vec![
            (256, 4, 1, size),
            (257, 4, 1, size),
            (258, 3, 1, 32),
            (259, 3, 1, 1),
            (262, 3, 1, 1),
            (273, 4, 1, pixel_offset),
            (277, 3, 1, 1),
            (278, 4, 1, size),
            (279, 4, 1, pixel_bytes),
            (339, 3, 1, 3),
        ];
        if let Some(text) = nodata {
            entries.push((42113, 2, text.len() as u32 + 1, nodata_offset));
        }
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for (tag, kind, count, value) in entries {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    fn tiff(inner: TileFormat) -> Box<TileFormat> {
        Box::new(inner)
    }

    #[test]
    fn operators_follow_the_upstream_parsers() {
        // Row-major 2x2: z = 0 is [1, 2], z = 1 is [3, 4].
        let bytes = float_tiff(2, &[1.0, 2.0, 3.0, 4.0], None);
        let parse = |format: TileFormat| format.parse(&bytes, 2).unwrap();
        assert_eq!(parse(TileFormat::Tiff), vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(parse(TileFormat::FlipX(tiff(TileFormat::Tiff))), vec![2.0, 1.0, 4.0, 3.0]);
        assert_eq!(parse(TileFormat::FlipZ(tiff(TileFormat::Tiff))), vec![3.0, 4.0, 1.0, 2.0]);
        assert_eq!(parse(TileFormat::SwapAxes(tiff(TileFormat::Tiff))), vec![1.0, 3.0, 2.0, 4.0]);
        assert_eq!(parse(TileFormat::Add(tiff(TileFormat::Tiff), 0.5)), vec![1.5, 2.5, 3.5, 4.5]);
        assert_eq!(parse(TileFormat::Multiply(tiff(TileFormat::Tiff), 3.0)), vec![3.0, 6.0, 9.0, 12.0]);
        // `divide` multiplies by the reciprocal: 3 * (1/10) is 0.30000000000000004, not 0.3.
        let divided = parse(TileFormat::Divide(tiff(TileFormat::Tiff), 10.0));
        assert_eq!(divided[2], 3.0 * (1.0 / 10.0));
        assert_ne!(divided[2], 0.3);
    }

    #[test]
    fn gdal_nodata_becomes_nan_after_widening_and_resolution_is_enforced() {
        let bytes = float_tiff(2, &[-9999.0, 0.1, 5.0, -9999.0], Some(" -9999.0"));
        let values = TileFormat::Tiff.parse(&bytes, 2).unwrap();
        assert!(values[0].is_nan() && values[3].is_nan());
        assert_eq!(values[1], f64::from(0.1f32));
        // A no-data value that a float cannot hold exactly never matches, as upstream.
        let bytes = float_tiff(2, &[0.1, 0.0, 0.0, 0.0], Some("0.100"));
        assert_eq!(TileFormat::Tiff.parse(&bytes, 2).unwrap()[0], f64::from(0.1f32));
        assert!(TileFormat::Tiff.parse(&float_tiff(2, &[0.0; 4], None), 4).is_err());
        assert!(TileFormat::Tiff.parse(&float_tiff(2, &[0.0; 4], Some("none!")), 2).is_err());
    }
}
