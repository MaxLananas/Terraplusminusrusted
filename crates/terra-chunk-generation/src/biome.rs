//! TerraPlusMinus biomes: the Köppen climate raster (`KoppenClimateData`) and its mapping to
//! Minecraft biomes (`CustomBiomeProvider.koppenDataToBukkitBiome`).

use std::sync::{Arc, OnceLock};
use terra_codec::DecodeError;

const COLUMNS: u32 = 43_200;
const ROWS: u32 = 21_600;
const SAMPLES: usize = (COLUMNS as usize) * (ROWS as usize);
const SCALE: f64 = 120.0;

static EMBEDDED_KOPPEN: &[u8] = include_bytes!("../resources/koppen_map.gz");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Biome {
    Ocean,
    Jungle,
    BambooJungle,
    SparseJungle,
    Desert,
    Savanna,
    Plains,
    SunflowerPlains,
    Beach,
    WindsweptGravellyHills,
    FlowerForest,
    WindsweptHills,
    SavannaPlateau,
    WoodedBadlands,
    SnowyTaiga,
    OldGrowthPineTaiga,
    Swamp,
    OldGrowthSpruceTaiga,
    Forest,
    DarkForest,
    Taiga,
    SnowySlopes,
    SnowyPlains,
    IceSpikes,
    StonyPeaks,
    FrozenPeaks,
}

impl Biome {
    pub const ALL: [Biome; 26] = [
        Biome::Ocean,
        Biome::Jungle,
        Biome::BambooJungle,
        Biome::SparseJungle,
        Biome::Desert,
        Biome::Savanna,
        Biome::Plains,
        Biome::SunflowerPlains,
        Biome::Beach,
        Biome::WindsweptGravellyHills,
        Biome::FlowerForest,
        Biome::WindsweptHills,
        Biome::SavannaPlateau,
        Biome::WoodedBadlands,
        Biome::SnowyTaiga,
        Biome::OldGrowthPineTaiga,
        Biome::Swamp,
        Biome::OldGrowthSpruceTaiga,
        Biome::Forest,
        Biome::DarkForest,
        Biome::Taiga,
        Biome::SnowySlopes,
        Biome::SnowyPlains,
        Biome::IceSpikes,
        Biome::StonyPeaks,
        Biome::FrozenPeaks,
    ];

    pub fn resource_name(self) -> &'static str {
        match self {
            Self::Ocean => "minecraft:ocean",
            Self::Jungle => "minecraft:jungle",
            Self::BambooJungle => "minecraft:bamboo_jungle",
            Self::SparseJungle => "minecraft:sparse_jungle",
            Self::Desert => "minecraft:desert",
            Self::Savanna => "minecraft:savanna",
            Self::Plains => "minecraft:plains",
            Self::SunflowerPlains => "minecraft:sunflower_plains",
            Self::Beach => "minecraft:beach",
            Self::WindsweptGravellyHills => "minecraft:windswept_gravelly_hills",
            Self::FlowerForest => "minecraft:flower_forest",
            Self::WindsweptHills => "minecraft:windswept_hills",
            Self::SavannaPlateau => "minecraft:savanna_plateau",
            Self::WoodedBadlands => "minecraft:wooded_badlands",
            Self::SnowyTaiga => "minecraft:snowy_taiga",
            Self::OldGrowthPineTaiga => "minecraft:old_growth_pine_taiga",
            Self::Swamp => "minecraft:swamp",
            Self::OldGrowthSpruceTaiga => "minecraft:old_growth_spruce_taiga",
            Self::Forest => "minecraft:forest",
            Self::DarkForest => "minecraft:dark_forest",
            Self::Taiga => "minecraft:taiga",
            Self::SnowySlopes => "minecraft:snowy_slopes",
            Self::SnowyPlains => "minecraft:snowy_plains",
            Self::IceSpikes => "minecraft:ice_spikes",
            Self::StonyPeaks => "minecraft:stony_peaks",
            Self::FrozenPeaks => "minecraft:frozen_peaks",
        }
    }

    pub fn from_resource_name(name: &str) -> Option<Self> {
        let qualified = if name.contains(':') { name.to_owned() } else { format!("minecraft:{name}") };
        Self::ALL.into_iter().find(|biome| biome.resource_name().eq_ignore_ascii_case(&qualified))
    }

    /// `koppenDataToBukkitBiome((int) value)`.
    pub fn from_koppen(class: u8) -> Self {
        match class {
            0 => Self::Ocean,
            1 | 12 => Self::Jungle,
            2 => Self::BambooJungle,
            3 | 11 => Self::SparseJungle,
            4 | 5 | 7 => Self::Desert,
            6 => Self::Savanna,
            9 => Self::SunflowerPlains,
            10 => Self::Beach,
            13 => Self::WindsweptGravellyHills,
            14 | 15 => Self::FlowerForest,
            16 => Self::WindsweptHills,
            17 => Self::SavannaPlateau,
            18 => Self::WoodedBadlands,
            19 => Self::SnowyTaiga,
            20 => Self::OldGrowthPineTaiga,
            21 | 22 => Self::Swamp,
            23 | 24 => Self::OldGrowthSpruceTaiga,
            25 => Self::Forest,
            26 => Self::DarkForest,
            27 => Self::Taiga,
            28 => Self::SnowySlopes,
            29 => Self::SnowyPlains,
            30 => Self::IceSpikes,
            _ => Self::Plains,
        }
    }
}

/// Appends the runs of `bytes`, which start at sample `position`, to a run-length encoding whose
/// last run may continue into `bytes` (upstream `RLEByteArray.Builder.append`, one slice at a time).
fn append_runs(run_starts: &mut Vec<u32>, run_values: &mut Vec<u8>, position: u32, bytes: &[u8]) {
    let mut index = 0;
    while index < bytes.len() {
        let value = bytes[index];
        if run_values.last() != Some(&value) {
            run_starts.push(position.wrapping_add(index as u32));
            run_values.push(value);
        }
        // Skip the rest of the run eight bytes at a time (runs average ~190 bytes).
        let splat = u64::from_ne_bytes([value; 8]);
        index += 1;
        while let Some(word) = bytes.get(index..index + 8)
            && <[u8; 8]>::try_from(word).ok().map(u64::from_ne_bytes) == Some(splat)
        {
            index += 8;
        }
        while bytes.get(index) == Some(&value) {
            index += 1;
        }
    }
}

/// The 30-arc-second Köppen-Geiger raster, held as run-length encoded rows.
#[derive(Debug)]
pub struct KoppenMap {
    run_starts: Vec<u32>,
    run_values: Vec<u8>,
}

impl KoppenMap {
    /// The compressed bundled raster, as fingerprinted.
    pub fn embedded_bytes() -> &'static [u8] {
        EMBEDDED_KOPPEN
    }

    /// Decodes the raster bundled from TerraPlusMinus; decoded once per process.
    pub fn embedded() -> Result<Arc<KoppenMap>, DecodeError> {
        static MAP: OnceLock<Result<Arc<KoppenMap>, DecodeError>> = OnceLock::new();
        MAP.get_or_init(|| Self::from_gzip(EMBEDDED_KOPPEN).map(Arc::new)).clone()
    }

    pub fn from_gzip(compressed: &[u8]) -> Result<Self, DecodeError> {
        let mut run_starts = Vec::new();
        let mut run_values = Vec::new();
        let mut position: u32 = 0;
        let mut sink = |bytes: &[u8]| {
            append_runs(&mut run_starts, &mut run_values, position, bytes);
            position = position.wrapping_add(bytes.len() as u32);
            Ok(())
        };
        let size = terra_codec::gunzip_streaming(compressed, SAMPLES, &mut sink)?;
        if size != SAMPLES {
            return Err(DecodeError::Truncated);
        }
        run_starts.shrink_to_fit();
        run_values.shrink_to_fit();
        Ok(Self { run_starts, run_values })
    }

    pub fn runs(&self) -> usize {
        self.run_values.len()
    }

    /// `KoppenClimateData.get` at a geographic coordinate; `None` outside |lon| <= 180, |lat| <= 90
    /// (where upstream throws `OutOfProjectionBoundsException`).
    pub fn class_at(&self, longitude: f64, latitude: f64) -> Option<u8> {
        if longitude.abs() > 180.0 || latitude.abs() > 90.0 {
            return None;
        }
        let x = ((longitude + 180.0) * SCALE).floor() as i32;
        let y = ((90.0 - latitude) * SCALE).floor() as i32;
        if x < 0 || y < 0 || x >= COLUMNS as i32 || y >= ROWS as i32 {
            return Some(0);
        }
        let index = (y as u32) * COLUMNS + x as u32;
        let run = self.run_starts.partition_point(|&start| start <= index).saturating_sub(1);
        Some(self.run_values.get(run).copied().unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::append_runs;

    /// The word-skipping run builder must produce exactly the runs of the byte-wise upstream
    /// builder, whatever the run lengths and however the stream is sliced.
    #[test]
    fn append_runs_matches_the_bytewise_builder() {
        let mut state = 0x9E37_79B9u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for _case in 0..200 {
            let mut data = Vec::new();
            while data.len() < 3000 {
                let value = (next() % 4) as u8;
                let length = match next() % 4 { 0 => 1, 1 => 1 + next() % 9, 2 => 1 + next() % 40, _ => 1 + next() % 400 };
                data.extend(std::iter::repeat_n(value, length as usize));
            }
            let (mut expected_starts, mut expected_values) = (Vec::new(), Vec::new());
            for (position, &value) in data.iter().enumerate() {
                if expected_values.last() != Some(&value) {
                    expected_starts.push(position as u32);
                    expected_values.push(value);
                }
            }
            let (mut starts, mut values) = (Vec::new(), Vec::new());
            let mut position = 0usize;
            while position < data.len() {
                let length = (1 + next() as usize % 700).min(data.len() - position);
                append_runs(&mut starts, &mut values, position as u32, &data[position..position + length]);
                position += length;
            }
            assert_eq!(starts, expected_starts);
            assert_eq!(values, expected_values);
        }
    }
}
