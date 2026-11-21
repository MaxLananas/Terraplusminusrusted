//! Dymaxion family: `DymaxionProjection`, `ConformalDynmaxionProjection`, `BTEDymaxionProjection`.

use std::fmt;
use std::sync::{Arc, OnceLock};

use crate::java_math::{asin, atan, atan2, cos, sin, tan, to_degrees, to_radians};
use crate::{
    GeoCoordinate, GeographicProjection, OutOfProjectionBounds, ProjectedBounds, ProjectedCoordinate,
    ProjectionResult, check_longitude_latitude,
};

const ROOT3: f64 = 1.732_050_807_568_877_2;
const NEWTON_ITERATIONS: usize = 5;
const FACE_COUNT: usize = 22;

const VERTICES_DEGREES: [[f64; 2]; 12] = [
    [10.536199, 64.700000],
    [-5.245390, 2.300882],
    [58.157706, 10.447378],
    [122.300000, 39.100000],
    [-143.478490, 50.103201],
    [-67.132330, 23.717925],
    [36.521510, -50.103200],
    [112.867673, -23.717930],
    [174.754610, -2.300882],
    [-121.842290, -10.447350],
    [-57.700000, -39.100000],
    [-169.463800, -64.700000],
];

const FACE_VERTICES: [[usize; 3]; FACE_COUNT] = [
    [2, 1, 6],
    [1, 0, 2],
    [0, 1, 5],
    [1, 5, 10],
    [1, 6, 10],
    [7, 2, 6],
    [2, 3, 7],
    [3, 0, 2],
    [0, 3, 4],
    [4, 0, 5],
    [5, 4, 9],
    [9, 5, 10],
    [10, 9, 11],
    [11, 6, 10],
    [6, 7, 11],
    [8, 3, 7],
    [8, 3, 4],
    [8, 4, 9],
    [9, 8, 11],
    [7, 8, 11],
    [11, 6, 7],
    [3, 7, 8],
];

const CENTER_MAP_UNITS: [[f64; 2]; FACE_COUNT] = [
    [-3.0, 7.0],
    [-2.0, 5.0],
    [-1.0, 7.0],
    [2.0, 5.0],
    [4.0, 5.0],
    [-4.0, 1.0],
    [-3.0, -1.0],
    [-2.0, 1.0],
    [-1.0, -1.0],
    [0.0, 1.0],
    [1.0, -1.0],
    [2.0, 1.0],
    [3.0, -1.0],
    [4.0, 1.0],
    [5.0, -1.0],
    [-3.0, -5.0],
    [-1.0, -5.0],
    [1.0, -5.0],
    [2.0, -7.0],
    [-4.0, -7.0],
    [-5.0, -5.0],
    [-2.0, -7.0],
];

const FLIP_TRIANGLE: [bool; FACE_COUNT] = [
    true, false, true, false, false, true, false, true, false, true, false, true, false, true, false,
    true, true, true, false, false, true, false,
];

const FACE_ON_GRID: [i32; 33] = [
    -1, -1, 0, 1, 2, -1, -1, 3, -1, 4, -1, -1, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 20, 19, 15, 21, 16,
    -1, 17, 18, -1, -1, -1,
];

struct DymaxionConstants {
    arc: f64,
    z: f64,
    el6: f64,
    dve: f64,
    r: f64,
    center_map: [[f64; 2]; FACE_COUNT],
    centroids: [[f64; 3]; FACE_COUNT],
    rotation_matrices: [[[f64; 3]; 3]; FACE_COUNT],
    inverse_rotation_matrices: [[[f64; 3]; 3]; FACE_COUNT],
}

fn geo_to_spherical(longitude: f64, latitude: f64) -> [f64; 2] {
    [to_radians(longitude), to_radians(90.0 - latitude)]
}

fn spherical_to_geo(spherical: [f64; 2]) -> GeoCoordinate {
    GeoCoordinate::new(to_degrees(spherical[0]), 90.0 - to_degrees(spherical[1]))
}

fn spherical_to_cartesian(spherical: [f64; 2]) -> [f64; 3] {
    let sin_phi = sin(spherical[1]);
    [sin_phi * cos(spherical[0]), sin_phi * sin(spherical[0]), cos(spherical[1])]
}

fn cartesian_to_spherical(cartesian: [f64; 3]) -> [f64; 2] {
    let lambda = atan2(cartesian[1], cartesian[0]);
    let phi = atan2((cartesian[0] * cartesian[0] + cartesian[1] * cartesian[1]).sqrt(), cartesian[2]);
    [lambda, phi]
}

/// `MathUtils.produceZYZRotationMatrix`.
fn zyz_rotation_matrix(a: f64, b: f64, c: f64) -> [[f64; 3]; 3] {
    let (sin_a, cos_a) = (sin(a), cos(a));
    let (sin_b, cos_b) = (sin(b), cos(b));
    let (sin_c, cos_c) = (sin(c), cos(c));
    [
        [cos_a * cos_b * cos_c - sin_c * sin_a, -sin_a * cos_b * cos_c - sin_c * cos_a, cos_c * sin_b],
        [sin_c * cos_b * cos_a + cos_c * sin_a, cos_c * cos_a - sin_c * cos_b * sin_a, sin_c * sin_b],
        [-sin_b * cos_a, sin_b * sin_a, cos_b],
    ]
}

/// `MathUtils.matVecProdD`, accumulating from `0.0` in the same order as Java.
fn matrix_vector_product(matrix: &[[f64; 3]; 3], vector: [f64; 3]) -> [f64; 3] {
    let mut result = [0.0f64; 3];
    for (row, out) in matrix.iter().zip(result.iter_mut()) {
        for (m, v) in row.iter().zip(vector.iter()) {
            *out += m * v;
        }
    }
    result
}

fn y_rotation(spherical: [f64; 2], rotation: f64) -> [f64; 2] {
    let mut c = spherical_to_cartesian(spherical);
    let x = c[0];
    c[0] = c[2] * sin(rotation) + x * cos(rotation);
    c[2] = c[2] * cos(rotation) - x * sin(rotation);
    let magnitude = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
    c[0] /= magnitude;
    c[1] /= magnitude;
    c[2] /= magnitude;
    [atan2(c[1], c[0]), atan2((c[0] * c[0] + c[1] * c[1]).sqrt(), c[2])]
}

fn constants() -> &'static DymaxionConstants {
    static CONSTANTS: OnceLock<DymaxionConstants> = OnceLock::new();
    CONSTANTS.get_or_init(|| {
        let arc = 2.0 * asin((5.0 - 5f64.sqrt()).sqrt() / 10f64.sqrt());
        let z = (5.0 + 2.0 * 5f64.sqrt()).sqrt() / 15f64.sqrt();
        let el = 8f64.sqrt() / (5.0 + 5f64.sqrt()).sqrt();
        let el6 = el / 6.0;
        let dve = (3.0 + 5f64.sqrt()).sqrt() / (5.0 + 5f64.sqrt()).sqrt();
        let r = -3.0 * el6 / dve;

        let mut center_map = CENTER_MAP_UNITS;
        for entry in &mut center_map {
            entry[0] *= 0.5 * arc;
            entry[1] *= arc * ROOT3 / 12.0;
        }

        let mut vertices_spherical = [[0.0; 2]; 12];
        let mut vertices_cartesian = [[0.0; 3]; 12];
        for (i, vertex) in VERTICES_DEGREES.iter().enumerate() {
            vertices_spherical[i] = geo_to_spherical(vertex[0], vertex[1]);
            vertices_cartesian[i] = spherical_to_cartesian(vertices_spherical[i]);
        }

        let mut centroids = [[0.0; 3]; FACE_COUNT];
        let mut rotation_matrices = [[[0.0; 3]; 3]; FACE_COUNT];
        let mut inverse_rotation_matrices = [[[0.0; 3]; 3]; FACE_COUNT];
        for face in 0..FACE_COUNT {
            let [a, b, c] = FACE_VERTICES[face].map(|i| vertices_cartesian[i]);
            let x_sum = a[0] + b[0] + c[0];
            let y_sum = a[1] + b[1] + c[1];
            let z_sum = a[2] + b[2] + c[2];
            let magnitude = (x_sum * x_sum + y_sum * y_sum + z_sum * z_sum).sqrt();
            centroids[face] = [x_sum / magnitude, y_sum / magnitude, z_sum / magnitude];

            let [centroid_lambda, centroid_phi] = cartesian_to_spherical(centroids[face]);
            let vertex = vertices_spherical[FACE_VERTICES[face][0]];
            let v = y_rotation([vertex[0] - centroid_lambda, vertex[1]], -centroid_phi);

            rotation_matrices[face] =
                zyz_rotation_matrix(-centroid_lambda, -centroid_phi, (std::f64::consts::PI / 2.0) - v[0]);
            inverse_rotation_matrices[face] =
                zyz_rotation_matrix(v[0] - (std::f64::consts::PI / 2.0), centroid_phi, centroid_lambda);
        }

        DymaxionConstants {
            arc,
            z,
            el6,
            dve,
            r,
            center_map,
            centroids,
            rotation_matrices,
            inverse_rotation_matrices,
        }
    })
}

fn find_triangle_grid(constants: &DymaxionConstants, x: f64, y: f64) -> Option<usize> {
    let xp = x / constants.arc;
    let mut yp = y / (constants.arc * ROOT3);
    let row: i32;
    if yp > -0.25 {
        if yp < 0.25 {
            row = 1;
        } else if yp <= 0.75 {
            row = 0;
            yp = 0.5 - yp;
        } else {
            return None;
        }
    } else if yp >= -0.75 {
        row = 2;
        yp = -yp - 0.5;
    } else {
        return None;
    }
    yp += 0.25;
    let xr = xp - yp;
    let yr = xp + yp;
    let gx = xr.floor() as i32;
    let gy = yr.floor() as i32;
    // Java int arithmetic wraps; saturated casts of huge inputs must wrap identically.
    let column = gx.wrapping_mul(2).wrapping_add(i32::from(gy != gx)).wrapping_add(6);
    if !(0..11).contains(&column) {
        return None;
    }
    usize::try_from(FACE_ON_GRID[(row * 11 + column) as usize]).ok()
}

/// The planar triangle mapping that distinguishes the plain and conformal variants.
#[derive(Clone)]
enum TriangleMapping {
    Gnomonic,
    Conformal(Arc<InvertibleVectorField>),
}

#[derive(Clone)]
struct DymaxionCore {
    mapping: TriangleMapping,
}

impl DymaxionCore {
    fn find_triangle(&self, vector: [f64; 3]) -> usize {
        let constants = constants();
        let mut minimum = f64::MAX;
        let mut face = 0;
        for (i, centroid) in constants.centroids.iter().enumerate().take(20) {
            let xd = centroid[0] - vector[0];
            let yd = centroid[1] - vector[1];
            let zd = centroid[2] - vector[2];
            let distance_squared = xd * xd + yd * yd + zd * zd;
            if distance_squared < minimum {
                if distance_squared < 0.1 {
                    return i;
                }
                face = i;
                minimum = distance_squared;
            }
        }
        face
    }

    fn gnomonic_triangle_transform(vector: [f64; 3]) -> [f64; 2] {
        let c = constants();
        let s = c.z / vector[2];
        let xp = s * vector[0];
        let yp = s * vector[1];
        let a = atan((2.0 * yp / ROOT3 - c.el6) / c.dve);
        let b = atan((xp - yp / ROOT3 - c.el6) / c.dve);
        let cc = atan((-xp - yp / ROOT3 - c.el6) / c.dve);
        [0.5 * (b - cc), (2.0 * a - b - cc) / (2.0 * ROOT3)]
    }

    fn inverse_gnomonic_triangle_transform(xpp: f64, ypp: f64) -> [f64; 3] {
        let c = constants();
        let tan_a_offset = tan(ROOT3 * ypp + xpp);
        let tan_b_offset = tan(2.0 * xpp);
        let a_numerator = tan_a_offset * tan_a_offset + 1.0;
        let b_numerator = tan_b_offset * tan_b_offset + 1.0;
        let mut tan_a = tan_a_offset;
        let mut tan_b = tan_b_offset;
        let mut tan_c = 0.0;
        let mut a_denominator = 1.0;
        let mut b_denominator = 1.0;
        for _ in 0..NEWTON_ITERATIONS {
            let f = tan_a + tan_b + tan_c - c.r;
            let fp = a_numerator * a_denominator * a_denominator + b_numerator * b_denominator * b_denominator + 1.0;
            tan_c -= f / fp;
            a_denominator = 1.0 / (1.0 - tan_c * tan_a_offset);
            b_denominator = 1.0 / (1.0 - tan_c * tan_b_offset);
            tan_a = (tan_c + tan_a_offset) * a_denominator;
            tan_b = (tan_c + tan_b_offset) * b_denominator;
        }
        let yp = ROOT3 * (c.dve * tan_a + c.el6) / 2.0;
        let xp = c.dve * tan_b + yp / ROOT3 + c.el6;
        let xp_over_z = xp / c.z;
        let yp_over_z = yp / c.z;
        let z = 1.0 / (1.0 + xp_over_z * xp_over_z + yp_over_z * yp_over_z).sqrt();
        [z * xp_over_z, z * yp_over_z, z]
    }

    fn triangle_transform(&self, vector: [f64; 3]) -> [f64; 2] {
        let gnomonic = Self::gnomonic_triangle_transform(vector);
        match &self.mapping {
            TriangleMapping::Gnomonic => gnomonic,
            TriangleMapping::Conformal(field) => {
                let arc = constants().arc;
                let (x, y) = (gnomonic[0], gnomonic[1]);
                let mut c = [x / arc + 0.5, y / arc + ROOT3 / 6.0];
                c = field.apply_newtons_method(x, y, c[0], c[1], 5);
                c[0] -= 0.5;
                c[1] -= ROOT3 / 6.0;
                c[0] *= arc;
                c[1] *= arc;
                c
            }
        }
    }

    fn inverse_triangle_transform(&self, x: f64, y: f64) -> [f64; 3] {
        match &self.mapping {
            TriangleMapping::Gnomonic => Self::inverse_gnomonic_triangle_transform(x, y),
            TriangleMapping::Conformal(field) => {
                let arc = constants().arc;
                let mut x = x / arc;
                let mut y = y / arc;
                x += 0.5;
                y += ROOT3 / 6.0;
                let c = field.interpolated_vector(x, y);
                Self::inverse_gnomonic_triangle_transform(c[0], c[1])
            }
        }
    }

    fn from_geo(&self, longitude: f64, latitude: f64) -> ProjectionResult<[f64; 2]> {
        check_longitude_latitude(longitude, latitude)?;
        let c = constants();
        let mut vector = spherical_to_cartesian(geo_to_spherical(longitude, latitude));
        let mut face = self.find_triangle(vector);
        let rotated = matrix_vector_product(&c.rotation_matrices[face], vector);
        let mut projected = self.triangle_transform(rotated);
        if FLIP_TRIANGLE[face] {
            projected[0] = -projected[0];
            projected[1] = -projected[1];
        }
        vector[0] = projected[0];
        if ((face == 15 && vector[0] > projected[1] * ROOT3) || face == 14) && vector[0] > 0.0 {
            projected[0] = 0.5 * vector[0] - 0.5 * ROOT3 * projected[1];
            projected[1] = 0.5 * ROOT3 * vector[0] + 0.5 * projected[1];
            face += 6;
        }
        projected[0] += c.center_map[face][0];
        projected[1] += c.center_map[face][1];
        Ok(projected)
    }

    fn to_geo(&self, mut x: f64, mut y: f64) -> ProjectionResult<GeoCoordinate> {
        let c = constants();
        let face = find_triangle_grid(c, x, y).ok_or(OutOfProjectionBounds)?;
        x -= c.center_map[face][0];
        y -= c.center_map[face][1];
        let outside = match face {
            14 => x > 0.0,
            20 => -y * ROOT3 > x,
            15 => x > 0.0 && x > y * ROOT3,
            21 => x < 0.0 || -y * ROOT3 > x,
            _ => false,
        };
        if outside {
            return Err(OutOfProjectionBounds);
        }
        if FLIP_TRIANGLE[face] {
            x = -x;
            y = -y;
        }
        let vector = self.inverse_triangle_transform(x, y);
        let rotated = matrix_vector_product(&c.inverse_rotation_matrices[face], vector);
        Ok(spherical_to_geo(cartesian_to_spherical(rotated)))
    }
}

/// Plain Dymaxion (Airocean / Fuller) projection.
#[derive(Clone)]
pub struct DymaxionProjection {
    core: DymaxionCore,
}

impl Default for DymaxionProjection {
    fn default() -> Self {
        Self { core: DymaxionCore { mapping: TriangleMapping::Gnomonic } }
    }
}

impl fmt::Debug for DymaxionProjection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Dymaxion")
    }
}

impl GeographicProjection for DymaxionProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        self.core.to_geo(point.x, point.y)
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let [x, y] = self.core.from_geo(point.longitude, point.latitude)?;
        Ok(ProjectedCoordinate::new(x, y))
    }

    fn meters_per_unit(&self) -> f64 {
        let arc = constants().arc;
        (510_100_000_000_000.0 / (20.0 * ROOT3 * arc * arc / 4.0)).sqrt()
    }

    fn bounds(&self) -> ProjectedBounds {
        let arc = constants().arc;
        ProjectedBounds { min_x: -3.0 * arc, min_y: -0.75 * arc * ROOT3, max_x: 2.5 * arc, max_y: 0.75 * arc * ROOT3 }
    }

    fn upright(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VectorFieldError {
    Decode(String),
    WrongSize { expected: usize, actual: usize },
}

impl fmt::Display for VectorFieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(reason) => write!(f, "failed to decode conformal.gz: {reason}"),
            Self::WrongSize { expected, actual } => {
                write!(f, "conformal.gz holds {actual} bytes, expected {expected}")
            }
        }
    }
}

impl std::error::Error for VectorFieldError {}

const SIDE_LENGTH: usize = 256;
const VECTOR_SCALE_FACTOR: f64 = 1.0 / 1.147_397_973_019_293_4;

/// Built-in `conformal.gz` from TerraMinusMinus (MIT, see `resources/NOTICE`).
const CONFORMAL_FIELD_GZIP: &[u8] = include_bytes!("../resources/conformal.gz");

/// `ConformalDynmaxionProjection.InvertableVectorField`, stored as triangular rows `u` of length
/// `SIDE_LENGTH + 1 - u`.
struct InvertibleVectorField {
    vx: Vec<f64>,
    vy: Vec<f64>,
    row_offsets: Vec<usize>,
}

impl InvertibleVectorField {
    fn load() -> Result<Self, VectorFieldError> {
        let samples = (SIDE_LENGTH + 1) * (SIDE_LENGTH + 2) / 2;
        let expected = samples * 16;
        let bytes = terra_codec::gunzip(CONFORMAL_FIELD_GZIP, expected)
            .map_err(|error| VectorFieldError::Decode(error.to_string()))?;
        if bytes.len() != expected {
            return Err(VectorFieldError::WrongSize { expected, actual: bytes.len() });
        }
        let mut row_offsets = Vec::with_capacity(SIDE_LENGTH + 1);
        let mut offset = 0;
        for u in 0..=SIDE_LENGTH {
            row_offsets.push(offset);
            offset += SIDE_LENGTH + 1 - u;
        }
        let mut vx = vec![0.0; samples];
        let mut vy = vec![0.0; samples];
        let mut values = bytes.chunks_exact(8).map(|chunk| {
            let mut raw = [0u8; 8];
            raw.copy_from_slice(chunk);
            f64::from_be_bytes(raw) * VECTOR_SCALE_FACTOR
        });
        for v in 0..=SIDE_LENGTH {
            for row_offset in &row_offsets[..=SIDE_LENGTH - v] {
                let index = row_offset + v;
                match (values.next(), values.next()) {
                    (Some(x), Some(y)) => {
                        vx[index] = x;
                        vy[index] = y;
                    }
                    _ => return Err(VectorFieldError::WrongSize { expected, actual: bytes.len() }),
                }
            }
        }
        Ok(Self { vx, vy, row_offsets })
    }

    fn shared() -> Result<Arc<Self>, VectorFieldError> {
        static FIELD: OnceLock<Result<Arc<InvertibleVectorField>, VectorFieldError>> = OnceLock::new();
        FIELD.get_or_init(|| Self::load().map(Arc::new)).clone()
    }

    fn sample(&self, u: usize, v: usize) -> (f64, f64) {
        let index = self.row_offsets[u] + v;
        (self.vx[index], self.vy[index])
    }

    fn interpolated_vector(&self, x: f64, y: f64) -> [f64; 6] {
        let side = SIDE_LENGTH as f64;
        let x = x * side;
        let mut y = y * side;
        let v = 2.0 * y / ROOT3;
        let u = x - v * 0.5;
        let mut u1 = u as i32;
        let mut v1 = v as i32;
        let side_i = SIDE_LENGTH as i32;
        if u1 < 0 {
            u1 = 0;
        } else if u1 >= side_i {
            u1 = side_i - 1;
        }
        if v1 < 0 {
            v1 = 0;
        } else if v1 >= side_i - u1 {
            v1 = side_i - u1 - 1;
        }
        let (u1f, v1f) = (f64::from(u1), f64::from(v1));
        let (uu, vv) = (u1 as usize, v1 as usize);
        let (value1, value2, value3, x3, y3, flip);
        if y < -ROOT3 * (x - u1f - v1f - 1.0) || v1 == side_i - u1 - 1 {
            value1 = self.sample(uu, vv);
            value2 = self.sample(uu, vv + 1);
            value3 = self.sample(uu + 1, vv);
            y3 = 0.5 * ROOT3 * v1f;
            x3 = (u1f + 1.0) + 0.5 * v1f;
            flip = 1.0;
        } else {
            value1 = self.sample(uu, vv + 1);
            value2 = self.sample(uu + 1, vv);
            value3 = self.sample(uu + 1, vv + 1);
            flip = -1.0;
            y = -y;
            y3 = -(0.5 * ROOT3 * (v1f + 1.0));
            x3 = (u1f + 1.0) + 0.5 * (v1f + 1.0);
        }
        let w1 = -(y - y3) / ROOT3 - (x - x3);
        let w2 = 2.0 * (y - y3) / ROOT3;
        let w3 = 1.0 - w1 - w2;
        let ((valx1, valy1), (valx2, valy2), (valx3, valy3)) = (value1, value2, value3);
        [
            valx1 * w1 + valx2 * w2 + valx3 * w3,
            valy1 * w1 + valy2 * w2 + valy3 * w3,
            (valx3 - valx1) * side,
            side * flip * (2.0 * valx2 - valx1 - valx3) / ROOT3,
            (valy3 - valy1) * side,
            side * flip * (2.0 * valy2 - valy1 - valy3) / ROOT3,
        ]
    }

    fn apply_newtons_method(&self, expected_f: f64, expected_g: f64, mut x_estimate: f64, mut y_estimate: f64, iterations: usize) -> [f64; 2] {
        for _ in 0..iterations {
            let c = self.interpolated_vector(x_estimate, y_estimate);
            let f = c[0] - expected_f;
            let g = c[1] - expected_g;
            let (dfdx, dfdy, dgdx, dgdy) = (c[2], c[3], c[4], c[5]);
            let determinant = 1.0 / (dfdx * dgdy - dfdy * dgdx);
            x_estimate -= determinant * (dgdy * f - dfdy * g);
            y_estimate -= determinant * (-dgdx * f + dfdx * g);
        }
        [x_estimate, y_estimate]
    }
}

/// Conformal Dymaxion: Dymaxion with the conformal vector field applied inside each face.
#[derive(Clone)]
pub struct ConformalDymaxionProjection {
    core: DymaxionCore,
}

impl ConformalDymaxionProjection {
    pub fn new() -> Result<Self, VectorFieldError> {
        Ok(Self { core: DymaxionCore { mapping: TriangleMapping::Conformal(InvertibleVectorField::shared()?) } })
    }
}

impl fmt::Debug for ConformalDymaxionProjection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Conformal Dymaxion")
    }
}

impl GeographicProjection for ConformalDymaxionProjection {
    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        self.core.to_geo(point.x, point.y)
    }

    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let [x, y] = self.core.from_geo(point.longitude, point.latitude)?;
        Ok(ProjectedCoordinate::new(x, y))
    }

    fn meters_per_unit(&self) -> f64 {
        (40_075_017.0 / (2.0 * std::f64::consts::PI)) / VECTOR_SCALE_FACTOR
    }

    fn bounds(&self) -> ProjectedBounds {
        DymaxionProjection::default().bounds()
    }

    fn upright(&self) -> bool {
        false
    }
}

const THETA_DEGREES: f64 = -150.0;
const BERING_X: f64 = -0.342_042_096_011_833_9;
const BERING_Y: f64 = -0.322_211_064_085_279;
const ARCTIC_Y: f64 = -0.2;
const ALEUTIAN_Y: f64 = -0.500_044_680_549_252_6;
const ALEUTIAN_XL: f64 = -0.514_923_127_975_750_7;
const ALEUTIAN_XR: f64 = -0.45;

/// BuildTheEarth's conformal Dymaxion with Eurasia rotated next to North America.
#[derive(Clone)]
pub struct BteDymaxionProjection {
    conformal: ConformalDymaxionProjection,
    sin_theta: f64,
    cos_theta: f64,
    arctic_m: f64,
    arctic_b: f64,
    aleutian_m: f64,
    aleutian_b: f64,
}

impl BteDymaxionProjection {
    pub fn new() -> Result<Self, VectorFieldError> {
        let arc = constants().arc;
        let theta = to_radians(THETA_DEGREES);
        let arctic_m = (ARCTIC_Y - ROOT3 * arc / 4.0) / (BERING_X - -0.5 * arc);
        let aleutian_m = (BERING_Y - ALEUTIAN_Y) / (BERING_X - ALEUTIAN_XR);
        Ok(Self {
            conformal: ConformalDymaxionProjection::new()?,
            sin_theta: sin(theta),
            cos_theta: cos(theta),
            arctic_m,
            arctic_b: ARCTIC_Y - arctic_m * BERING_X,
            aleutian_m,
            aleutian_b: BERING_Y - aleutian_m * BERING_X,
        })
    }

    fn is_eurasian_part(&self, x: f64, y: f64) -> bool {
        let arc = constants().arc;
        if x > 0.0 {
            return false;
        }
        if x < -0.5 * arc {
            return true;
        }
        if y > ROOT3 * arc / 4.0 {
            return x < 0.0;
        }
        if y < ALEUTIAN_Y {
            return y < (ALEUTIAN_Y + ALEUTIAN_XL) - x;
        }
        if y > BERING_Y {
            if y < ARCTIC_Y {
                return x < BERING_X;
            }
            return y < self.arctic_m * x + self.arctic_b;
        }
        y > self.aleutian_m * x + self.aleutian_b
    }
}

impl fmt::Debug for BteDymaxionProjection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BuildTheEarth Conformal Dymaxion")
    }
}

impl GeographicProjection for BteDymaxionProjection {
    fn from_geo(&self, point: GeoCoordinate) -> ProjectionResult<ProjectedCoordinate> {
        let arc = constants().arc;
        let [mut x, mut y] = self.conformal.core.from_geo(point.longitude, point.latitude)?;
        let eurasia = self.is_eurasian_part(x, y);
        y -= 0.75 * arc * ROOT3;
        if eurasia {
            x += arc;
            let t = x;
            x = self.cos_theta * x - self.sin_theta * y;
            y = self.sin_theta * t + self.cos_theta * y;
        } else {
            x -= arc;
        }
        Ok(ProjectedCoordinate::new(y, -x))
    }

    fn to_geo(&self, point: ProjectedCoordinate) -> ProjectionResult<GeoCoordinate> {
        let arc = constants().arc;
        let (mut x, mut y) = (point.x, point.y);
        let eurasia = if y < 0.0 {
            x > 0.0
        } else if y > arc / 2.0 {
            x > -ROOT3 * arc / 2.0
        } else {
            y * -ROOT3 < x
        };
        let t = x;
        x = -y;
        y = t;
        if eurasia {
            let t = x;
            x = self.cos_theta * x + self.sin_theta * y;
            y = self.cos_theta * y - self.sin_theta * t;
            x -= arc;
        } else {
            x += arc;
        }
        y += 0.75 * arc * ROOT3;
        if eurasia != self.is_eurasian_part(x, y) {
            return Err(OutOfProjectionBounds);
        }
        self.conformal.core.to_geo(x, y)
    }

    fn meters_per_unit(&self) -> f64 {
        self.conformal.meters_per_unit()
    }

    fn bounds(&self) -> ProjectedBounds {
        let arc = constants().arc;
        ProjectedBounds { min_x: -1.5 * arc * ROOT3, min_y: -1.5 * arc, max_x: 3.0 * arc, max_y: ROOT3 * arc }
    }

    fn upright(&self) -> bool {
        self.conformal.upright()
    }
}
