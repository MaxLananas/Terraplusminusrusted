//! `VectorGeometry` and its rasterisers (`NarrowLine`, `SharpLine`, `WideLine`, `FillPolygon`,
//! `DistancePolygon`), ported with Java's integer wrap-around, saturating casts and `Math`
//! semantics so every chunk column matches TerraMinusMinus.

use crate::bounds::{Bounded, Bounds2d};
use crate::bvh::Bvh;
use crate::draw::{DrawFunction, DrawTarget};
use crate::interval_tree::IntervalTree;
use std::cmp::Ordering;
use std::sync::Arc;
use terra_codec::java_double_compare;
use terra_elevation::numeric::{ceil_i, floor_i, java_max, java_min, lerp};

/// A projected point: `x` is the block x coordinate, `z` the block z coordinate.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub x0: f64,
    pub z0: f64,
    pub x1: f64,
    pub z1: f64,
}

impl Segment {
    pub fn min_x(&self) -> f64 {
        java_min(self.x0, self.x1)
    }

    pub fn max_x(&self) -> f64 {
        java_max(self.x0, self.x1)
    }
}

impl Bounded for Segment {
    fn bounds(&self) -> Bounds2d {
        Bounds2d { min_x: java_min(self.x0, self.x1), max_x: java_max(self.x0, self.x1), min_z: java_min(self.z0, self.z1), max_z: java_max(self.z0, self.z1) }
    }
}

fn segments_of(lines: &[Vec<Point>]) -> Vec<Segment> {
    let mut segments = Vec::new();
    for line in lines {
        for pair in line.windows(2) {
            segments.push(Segment { x0: pair[0].x, z0: pair[0].z, x1: pair[1].x, z1: pair[1].z });
        }
    }
    segments
}

/// A projected polygon: the outer ring first, then holes. Rings are closed (first == last).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedPolygon {
    pub outer: Vec<Point>,
    pub inner: Vec<Vec<Point>>,
}

#[derive(Debug)]
enum Shape {
    Narrow(Bvh<Segment>),
    Sharp(Bvh<Segment>),
    Wide { segments: Bvh<Segment>, radius: f64 },
    Fill(IntervalTree),
    Distance { edges: IntervalTree, max_dist: i32 },
}

#[derive(Debug)]
pub struct VectorGeometry {
    id: String,
    layer: f64,
    draw: Arc<DrawFunction>,
    shape: Shape,
    bounds: Bounds2d,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Narrow,
    Sharp,
}

impl VectorGeometry {
    pub fn line(kind: LineKind, id: String, layer: f64, draw: Arc<DrawFunction>, lines: &[Vec<Point>]) -> Self {
        let segments = Bvh::of(segments_of(lines));
        let bounds = segments.bounds().expand(1.0);
        let shape = match kind {
            LineKind::Narrow => Shape::Narrow(segments),
            LineKind::Sharp => Shape::Sharp(segments),
        };
        Self { id, layer, draw, shape, bounds }
    }

    pub fn wide_line(id: String, layer: f64, draw: Arc<DrawFunction>, lines: &[Vec<Point>], radius: f64) -> Self {
        let segments = Bvh::of(segments_of(lines));
        let inner = segments.bounds();
        let bounds = Bounds2d { min_x: inner.min_x - radius, max_x: inner.max_x + radius, min_z: inner.min_z - radius, max_z: inner.max_z + radius };
        Self { id, layer, draw, shape: Shape::Wide { segments, radius }, bounds }
    }

    /// `max_dist` is `Some` for a `DistancePolygon`, `None` for a `FillPolygon`.
    pub fn polygon(id: String, layer: f64, draw: Arc<DrawFunction>, polygons: &[ProjectedPolygon], max_dist: Option<i32>) -> Self {
        let mut bounds = Bounds2d { min_x: f64::INFINITY, max_x: f64::NEG_INFINITY, min_z: f64::INFINITY, max_z: f64::NEG_INFINITY };
        let mut segments = Vec::new();
        for polygon in polygons {
            for point in &polygon.outer {
                bounds = Bounds2d { min_x: java_min(bounds.min_x, point.x), max_x: java_max(bounds.max_x, point.x), min_z: java_min(bounds.min_z, point.z), max_z: java_max(bounds.max_z, point.z) };
            }
            segments.extend(segments_of(std::slice::from_ref(&polygon.outer)));
            segments.extend(segments_of(&polygon.inner));
        }
        let edges = IntervalTree::new(segments);
        let shape = match max_dist {
            Some(max_dist) => Shape::Distance { edges, max_dist },
            None => Shape::Fill(edges),
        };
        Self { id, layer, draw, shape, bounds }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn layer(&self) -> f64 {
        self.layer
    }

    /// `VectorGeometry.compareTo`: layer by `Double.compare`, then id by `String.compareTo`
    /// (UTF-16 code units).
    pub fn draw_order(&self, other: &VectorGeometry) -> Ordering {
        java_double_compare(self.layer, other.layer).then_with(|| self.id.encode_utf16().cmp(other.id.encode_utf16()))
    }

    /// `apply(builder, chunkX, chunkZ, bounds)` where `bounds` is the chunk's block box.
    pub fn apply(&self, target: &mut dyn DrawTarget, chunk_x: i32, chunk_z: i32, chunk_bounds: &Bounds2d) {
        let base_x = chunk_x.wrapping_shl(4);
        let base_z = chunk_z.wrapping_shl(4);
        let mut plot = |x: i32, z: i32, weight: i32| {
            if (0..16).contains(&x) && (0..16).contains(&z) {
                self.draw.draw_onto(target, x as usize, z as usize, weight);
            }
        };
        match &self.shape {
            Shape::Narrow(segments) => segments.for_each_intersecting(chunk_bounds, &mut |segment| narrow_segment(segment, base_x, base_z, &mut plot)),
            Shape::Sharp(segments) => segments.for_each_intersecting(chunk_bounds, &mut |segment| sharp_segment(segment, base_x, base_z, &mut plot)),
            Shape::Wide { segments, radius } => {
                segments.for_each_intersecting(&chunk_bounds.expand(*radius), &mut |segment| wide_segment(segment, *radius, base_x, base_z, &mut plot));
            }
            Shape::Fill(edges) => fill_polygon(edges, base_x, base_z, &mut plot),
            Shape::Distance { edges, max_dist } => distance_polygon(edges, *max_dist, base_x, base_z, &mut plot),
        }
    }
}

impl Bounded for VectorGeometry {
    fn bounds(&self) -> Bounds2d {
        self.bounds
    }
}

fn narrow_segment(segment: &Segment, base_x: i32, base_z: i32, plot: &mut dyn FnMut(i32, i32, i32)) {
    let Segment { mut x0, z0, mut x1, z1 } = *segment;
    let difference = x1 - x0;
    let run = if difference.abs() < 0.01 { x1 + 0.01f64.copysign(difference) } else { x1 };
    let slope = (z1 - z0) / (run - x0);
    let offset = z0 - slope * x0;
    if x0 > x1 {
        std::mem::swap(&mut x0, &mut x1);
    }
    let start_x = floor_i(x0).wrapping_sub(base_x).max(0);
    let end_x = floor_i(x1).wrapping_sub(base_x).min(15);
    for x in start_x..=end_x {
        let real_x = java_max(f64::from(x.wrapping_add(base_x)), x0);
        let next_x = java_min(real_x + 1.0, x1);
        let mut from = floor_i(slope * real_x + offset).wrapping_sub(base_z);
        let mut to = floor_i(slope * next_x + offset).wrapping_sub(base_z);
        if from > to {
            std::mem::swap(&mut from, &mut to);
        }
        for z in from.max(0)..=to.min(15) {
            plot(x, z, 1);
        }
    }
}

/// Upstream steps at least half a block per iteration inside one chunk; the cap only guards
/// against arithmetic that could never terminate.
const SHARP_LINE_MAX_STEPS: usize = 1024;

fn sharp_segment(segment: &Segment, base_x: i32, base_z: i32, plot: &mut dyn FnMut(i32, i32, i32)) {
    let mut x0 = segment.x0 - f64::from(base_x);
    let mut x1 = segment.x1 - f64::from(base_x);
    let mut z0 = segment.z0 - f64::from(base_z);
    let mut z1 = segment.z1 - f64::from(base_z);
    let mut dx = x1 - x0;
    let mut dz = z1 - z0;
    let walk = |major0: &mut f64, major1: &mut f64, minor0: f64, delta_major: &mut f64, delta_minor: f64, plot: &mut dyn FnMut(f64, f64)| {
        if delta_major.abs() < 0.01 {
            *delta_major += 0.01f64.copysign(*delta_major);
        }
        let slope = delta_minor / *delta_major;
        let offset = minor0 - slope * *major0;
        if *major0 > *major1 {
            std::mem::swap(major0, major1);
        }
        let start = java_max(0.0, *major0);
        let end = java_min(*major1, 15.999);
        let step = (end - start) / f64::from(ceil_i(end - start));
        let mut major = start;
        let mut steps = 0;
        while major <= end && steps < SHARP_LINE_MAX_STEPS {
            let minor = slope * major + offset;
            if (0.0..16.0).contains(&minor) {
                plot(major, minor);
            }
            major += step;
            steps += 1;
        }
    };
    if dx.abs() >= dz.abs() {
        walk(&mut x0, &mut x1, z0, &mut dx, dz, &mut |x, z| plot(floor_i(x), floor_i(z), 1));
    } else {
        walk(&mut z0, &mut z1, x0, &mut dz, dx, &mut |z, x| plot(floor_i(x), floor_i(z), 1));
    }
}

/// `MathUtils.clamp(double)`: NaN passes through unchanged.
fn clamp_unit(value: f64) -> f64 {
    value.clamp(0.0, 1.0)
}

fn wide_segment(segment: &Segment, radius: f64, base_x: i32, base_z: i32, plot: &mut dyn FnMut(i32, i32, i32)) {
    let radius_sq = radius * radius;
    let lon0 = segment.x0 - f64::from(base_x);
    let lon1 = segment.x1 - f64::from(base_x);
    let lat0 = segment.z0 - f64::from(base_z);
    let lat1 = segment.z1 - f64::from(base_z);
    let min_x = floor_i(java_min(lon0, lon1) - radius).max(0);
    let max_x = ceil_i(java_max(lon0, lon1) + radius).min(16);
    let min_z = floor_i(java_min(lat0, lat1) - radius).max(0);
    let max_z = ceil_i(java_max(lat0, lat1) + radius).min(16);
    let length_sq = (lon1 - lon0) * (lon1 - lon0) + (lat1 - lat0) * (lat1 - lat0);
    for x in min_x..max_x {
        for z in min_z..max_z {
            let (fx, fz) = (f64::from(x), f64::from(z));
            let r = clamp_unit(((fx - lon0) * (lon1 - lon0) + (fz - lat0) * (lat1 - lat0)) / length_sq);
            let dx = (lon0 + r * (lon1 - lon0)) - fx;
            let dz = (lat0 + r * (lat1 - lat0)) - fz;
            let distance_sq = dx * dx + dz * dz;
            if distance_sq < radius_sq {
                plot(x, z, floor_i(radius - distance_sq.sqrt()));
            }
        }
    }
}

/// Upstream retries an odd crossing count (a vertex exactly on the scan line) at an offset drawn
/// from `ThreadLocalRandom`, so its output there is not reproducible. The same retry schedule
/// here draws the offset from a hash of the scan line, keeping generation deterministic.
fn retry_offset(position: i32, attempt: u32) -> f64 {
    let mut state = (u64::from(position as u32) << 32 | u64::from(attempt)).wrapping_add(0x9E37_79B9_7F4A_7C15);
    state = (state ^ (state >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state = (state ^ (state >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    state ^= state >> 31;
    0.45 + (state >> 11) as f64 / (1u64 << 53) as f64 * 0.1
}

/// `AbstractPolygon.getIntersectionPoints`: sorted z crossings of the scan line x = pos + 0.5.
fn intersection_points(edges: &IntervalTree, position: i32) -> Vec<f64> {
    let mut offset = 0.5;
    for attempt in 0..4u32 {
        let center = f64::from(position) + offset;
        let mut crossings = Vec::new();
        edges.for_each_containing(center, &mut |segment| crossings.push(lerp(segment.z0, segment.z1, (segment.x0 - center) / (segment.x0 - segment.x1))));
        if crossings.len() % 2 == 0 {
            crossings.sort_by(|a, b| java_double_compare(*a, *b));
            return crossings;
        }
        offset = retry_offset(position, attempt);
    }
    Vec::new()
}

/// `PMath.clamp(int, int, int)`.
fn clamp_int(value: i32, min: i32, max: i32) -> i32 {
    value.max(min).min(max)
}

fn fill_polygon(edges: &IntervalTree, base_x: i32, base_z: i32, plot: &mut dyn FnMut(i32, i32, i32)) {
    for x in 0..16_i32 {
        let points = intersection_points(edges, x.wrapping_add(base_x));
        for pair in points.chunks_exact(2) {
            let min = clamp_int(floor_i(pair[0]).wrapping_sub(base_z), 0, 16);
            let max = clamp_int(floor_i(pair[1]).wrapping_sub(base_z), 0, 16);
            for z in min..max {
                plot(x, z, 1);
            }
        }
    }
}

/// `Arrays.binarySearch(double[], double)`.
fn java_binary_search(values: &[f64], key: f64) -> i32 {
    let mut low: i64 = 0;
    let mut high: i64 = values.len() as i64 - 1;
    while low <= high {
        let middle = (low + high) / 2;
        let value = values[middle as usize];
        if value < key {
            low = middle + 1;
        } else if value > key {
            high = middle - 1;
        } else {
            let value_bits = canonical_bits(value);
            let key_bits = canonical_bits(key);
            match value_bits.cmp(&key_bits) {
                Ordering::Equal => return middle as i32,
                Ordering::Less => low = middle + 1,
                Ordering::Greater => high = middle - 1,
            }
        }
    }
    -(low as i32 + 1)
}

/// `Double.doubleToLongBits`: every NaN collapses to the canonical pattern.
fn canonical_bits(value: f64) -> i64 {
    if value.is_nan() { 0x7ff8_0000_0000_0000 } else { value.to_bits() as i64 }
}

/// `DistancePolygon.apply`: signed distance (positive inside) to the outline along x and z,
/// computed scan line by scan line in a sliding window of `2 * max_dist + 1` rows. Rows are
/// recycled without clearing, as upstream does, because stale entries can survive into results.
fn distance_polygon(edges: &IntervalTree, max_dist: i32, base_x: i32, base_z: i32, plot: &mut dyn FnMut(i32, i32, i32)) {
    let window = (max_dist as usize) * 2 + 1;
    let middle = max_dist as usize;
    let mut rows = vec![[0i32; 16]; window];
    for x in -max_dist..16 + max_dist {
        rows.rotate_left(1);
        let distances = &mut rows[window - 1];
        let points = intersection_points(edges, x.wrapping_add(base_x));
        if points.is_empty() {
            distances.fill(i32::MIN);
        } else {
            let mut index = java_binary_search(&points, f64::from(base_z.wrapping_sub(max_dist)));
            index ^= index >> 31;
            index -= index & 1;
            let mut index = index as usize;
            let max;
            if index < points.len() {
                let mut mask = 0i32;
                let mut min = floor_i(points[index]).wrapping_sub(base_z);
                index += 1;
                for z in 0..clamp_int(min, 0, 16) {
                    distances[z as usize] = z.wrapping_sub(min);
                }
                loop {
                    let Some(point) = points.get(index) else { break };
                    let current = floor_i(*point).wrapping_sub(base_z);
                    index += 1;
                    for z in clamp_int(min, 0, 16)..clamp_int(current, 0, 16) {
                        distances[z as usize] = z.wrapping_sub(min).min(current.wrapping_sub(z).wrapping_sub(1)) ^ mask;
                    }
                    min = current;
                    mask = !mask;
                    if !(index < points.len() && current <= base_x.wrapping_add(16)) {
                        break;
                    }
                }
                max = min;
            } else {
                max = floor_i(points[points.len() - 1]).wrapping_sub(base_z);
            }
            for z in clamp_int(max, 0, 16)..16 {
                distances[z as usize] = max.wrapping_sub(z).wrapping_sub(1);
            }
        }
        if x >= max_dist {
            #[allow(clippy::needless_range_loop, reason = "z indexes every row of the window, not only `rows[middle]`")]
            for z in 0..16 {
                let mut distance = rows[middle][z];
                let reach = distance.wrapping_abs();
                for dx in reach.wrapping_neg().max(-max_dist)..=reach.min(max_dist) {
                    if dx == 0 {
                        continue;
                    }
                    let mut neighbour = rows[(dx + max_dist) as usize][z];
                    neighbour = if distance > 0 {
                        (neighbour & !(neighbour >> 31)).wrapping_add(dx.wrapping_abs()).wrapping_sub(((neighbour as u32) >> 31) as i32)
                    } else {
                        (neighbour & (neighbour >> 31)).wrapping_sub(dx.wrapping_abs())
                    };
                    if neighbour.wrapping_abs() < distance.wrapping_abs() {
                        distance = neighbour;
                    }
                }
                if distance >= -max_dist {
                    plot(x - max_dist, z as i32, distance.min(max_dist));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::BlockState;

    #[derive(Default)]
    struct Recorder {
        water: Vec<(usize, usize, i32)>,
    }

    impl DrawTarget for Recorder {
        fn update_water_depth(&mut self, x: usize, z: usize, depth: i32) {
            self.water.push((x, z, depth));
        }
        fn update_ocean_depth(&mut self, x: usize, z: usize, depth: i32) {
            self.water.push((x, z, depth + 1000));
        }
        fn set_surface_block(&mut self, _: usize, _: usize, _: &BlockState) {}
        fn clear_tree_cover(&mut self, _: usize, _: usize) {}
    }

    fn chunk_bounds(chunk_x: i32, chunk_z: i32) -> Bounds2d {
        let (x, z) = (f64::from(chunk_x * 16), f64::from(chunk_z * 16));
        Bounds2d::of(x, x + 16.0, z, z + 16.0)
    }

    fn square(min: f64, max: f64) -> ProjectedPolygon {
        let corners = [(min, min), (max, min), (max, max), (min, max), (min, min)];
        ProjectedPolygon { outer: corners.iter().map(|&(x, z)| Point { x, z }).collect(), inner: Vec::new() }
    }

    #[test]
    fn binary_search_matches_java() {
        let values = [-3.0, -0.0, 0.0, 2.5, 2.5, 7.0];
        assert_eq!(java_binary_search(&values, 0.0), 2);
        assert_eq!(java_binary_search(&values, -0.0), 1);
        assert_eq!(java_binary_search(&values, 1.0), -4);
        assert_eq!(java_binary_search(&values, 9.0), -7);
        assert_eq!(java_binary_search(&[], 1.0), -1);
    }

    #[test]
    fn fill_covers_the_interior_of_negative_coordinates() {
        let geometry = VectorGeometry::polygon("lake".into(), 0.0, Arc::new(DrawFunction::Water), &[square(-20.0, -4.0)], None);
        let mut recorder = Recorder::default();
        geometry.apply(&mut recorder, -1, -1, &chunk_bounds(-1, -1));
        let mut cells: Vec<(usize, usize)> = recorder.water.iter().map(|&(x, z, _)| (x, z)).collect();
        cells.sort_unstable();
        let expected: Vec<(usize, usize)> = (0..12).flat_map(|x| (0..12).map(move |z| (x, z))).collect();
        assert_eq!(cells, expected);
    }

    #[test]
    fn retry_offsets_stay_in_the_upstream_range() {
        for position in [-5, 0, 77, i32::MAX] {
            for attempt in 0..3 {
                let offset = retry_offset(position, attempt);
                assert!((0.45..0.55).contains(&offset));
            }
        }
    }
}
