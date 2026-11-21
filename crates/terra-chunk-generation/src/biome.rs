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

/// The 30-arc-second Köppen-Geiger raster, held as run-length encoded rows.
#[derive(Debug)]
pub struct KoppenMap {
    run_starts: Vec<u32>,
    run_values: Vec<u8>,
}

impl KoppenMap {
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
            for &value in bytes {
                if run_values.last() != Some(&value) {
                    run_starts.push(position);
                    run_values.push(value);
                }
                position += 1;
            }
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
