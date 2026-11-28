//! `net.buildtheearth.terraminusminus.dataset.BlendMode`.

use crate::numeric::{floor_i, lerp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendMode {
    Near,
    Linear,
    Cubic,
}

impl BlendMode {
    /// Upstream `offset` field.
    pub fn offset(self) -> f64 {
        match self {
            Self::Near | Self::Linear => 0.0,
            Self::Cubic => -0.5,
        }
    }

    /// Upstream `size` field: how far beyond a sample point the blend reads raw values.
    pub fn size(self) -> i32 {
        match self {
            Self::Near => 0,
            Self::Linear => 2,
            Self::Cubic => 3,
        }
    }

    /// Upstream's enum constant names (`BlendMode.valueOf`): `NEAR`, `LINEAR`, `CUBIC`, exactly.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "NEAR" => Some(Self::Near),
            "LINEAR" => Some(Self::Linear),
            "CUBIC" => Some(Self::Cubic),
            _ => None,
        }
    }

    pub fn get(self, scaled_x: f64, scaled_z: f64, sampler: impl Fn(i32, i32) -> f64) -> f64 {
        match self {
            Self::Near => sampler(floor_i(scaled_x), floor_i(scaled_z)),
            Self::Linear => {
                let sample_x = floor_i(scaled_x);
                let sample_z = floor_i(scaled_z);
                let fx = scaled_x - f64::from(sample_x);
                let fz = scaled_z - f64::from(sample_z);
                let v00 = sampler(sample_x, sample_z);
                let v01 = sampler(sample_x, sample_z.wrapping_add(1));
                let v10 = sampler(sample_x.wrapping_add(1), sample_z);
                let v11 = sampler(sample_x.wrapping_add(1), sample_z.wrapping_add(1));
                lerp(lerp(v00, v01, fz), lerp(v10, v11, fz), fx)
            }
            Self::Cubic => cubic(scaled_x, scaled_z, sampler),
        }
    }
}

fn cubic(scaled_x: f64, scaled_z: f64, sampler: impl Fn(i32, i32) -> f64) -> f64 {
    let x = scaled_x - 0.5;
    let z = scaled_z - 0.5;
    let sample_x = floor_i(x);
    let sample_z = floor_i(z);
    let fx = x - f64::from(sample_x);
    let fz = z - f64::from(sample_z);
    let at = |dx: i32, dz: i32| sampler(sample_x.wrapping_add(dx), sample_z.wrapping_add(dz));
    let grid = [
        [at(0, 0), at(0, 1), at(0, 2)],
        [at(1, 0), at(1, 1), at(1, 2)],
        [at(2, 0), at(2, 1), at(2, 2)],
    ];
    let result = nine_point(fx, fz, &grid);
    let all_at_or_below_sea = grid.iter().flatten().all(|&value| value <= 0.0);
    if result > 0.0 && all_at_or_below_sea {
        return 0.0;
    }
    result
}

/// K.jpg's 9-point interpolation, evaluated in exactly the upstream operation order.
#[allow(clippy::similar_names)]
fn nine_point(fx: f64, fy: f64, v: &[[f64; 3]; 3]) -> f64 {
    let [[v00, v01, v02], [v10, v11, v12], [v20, v21, v22]] = *v;
    let x_fade = fx * fx * (3.0 - 2.0 * fx);
    let y_fade = fy * fy * (3.0 - 2.0 * fy);

    let v_aa = (v00 + v01 + v10 + v11) * 0.25;
    let v_ab = (v01 + v02 + v11 + v12) * 0.25;
    let v_ba = (v10 + v20 + v11 + v21) * 0.25;
    let v_bb = (v11 + v21 + v12 + v22) * 0.25;

    let v_aa_x = (v10 + v11) - (v00 + v01);
    let v_aa_y = (v01 + v11) - (v00 + v10);
    let v_ab_x = (v11 + v12) - (v01 + v02);
    let v_ab_y = (v02 + v12) - (v01 + v11);
    let v_ba_x = (v20 + v21) - (v10 + v11);
    let v_ba_y = (v11 + v21) - (v10 + v20);
    let v_bb_x = (v21 + v22) - (v11 + v12);
    let v_bb_y = (v12 + v22) - (v11 + v21);

    let fx = fx * 0.5;
    let fy = fy * 0.5;
    let ix = fx - 0.5;
    let iy = fy - 0.5;

    let blend_xa = (1.0 - x_fade) * (v_aa + v_aa_x * fx + v_aa_y * fy) + x_fade * (v_ba + v_ba_x * ix + v_ba_y * fy);
    let blend_xb = (1.0 - x_fade) * (v_ab + v_ab_x * fx + v_ab_y * iy) + x_fade * (v_bb + v_bb_x * ix + v_bb_y * iy);
    (1.0 - y_fade) * blend_xa + y_fade * blend_xb
}
