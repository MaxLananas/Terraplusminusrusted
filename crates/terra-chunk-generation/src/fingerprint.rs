//! The generation fingerprint persisted beside a world: every input that changes generated
//! blocks. A world generated under one fingerprint refuses to keep generating under another,
//! so new chunks never mismatch old ones at the seams.

use crate::generator::{GeneratorSettings, WorldHeight};
use std::fs;
use std::io::Write;
use std::path::Path;

/// Bump when generation output changes for identical inputs.
pub const GENERATION_ALGORITHM_VERSION: u32 = 1;
pub const FINGERPRINT_FILE_NAME: &str = "terraplusminus-generation.fingerprint";
const HEADER: &str = "terraplusminus-generation-fingerprint 1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationFingerprint {
    fields: Vec<(String, String)>,
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

impl GenerationFingerprint {
    /// `heights_config` and `biome_raster` are the exact bytes generation reads.
    pub fn compute(settings: &GeneratorSettings, world: WorldHeight, heights_config: &[u8], biome_raster: Option<&[u8]>) -> Self {
        let digest = |bytes: &[u8]| format!("{:016x}:{}", fnv1a64(bytes), bytes.len());
        let mut fields = vec![
            ("algorithm".to_owned(), GENERATION_ALGORITHM_VERSION.to_string()),
            ("projection".to_owned(), "bte_conformal_dymaxion".to_owned()),
            ("x_offset".to_owned(), settings.x_offset.to_string()),
            ("y_offset".to_owned(), settings.y_offset.to_string()),
            ("z_offset".to_owned(), settings.z_offset.to_string()),
            ("min_y".to_owned(), world.min_y.to_string()),
            ("max_y".to_owned(), world.max_y.to_string()),
            ("biome_dataset".to_owned(), settings.use_biome_dataset.to_string()),
            ("default_biome".to_owned(), settings.default_biome.resource_name().to_owned()),
            ("surface_material".to_owned(), settings.surface_material.resource_name().to_owned()),
            ("heights_config".to_owned(), digest(heights_config)),
            ("biome_raster".to_owned(), biome_raster.map_or_else(|| "none".to_owned(), digest)),
        ];
        // Fields added after the first release are written only when in use, so worlds created
        // before them keep matching while they stay unused.
        if !settings.surface_replacements.is_empty() {
            let rendered: Vec<String> = settings.surface_replacements.iter().map(|(id, block)| format!("{id}>{}", block.resource_name())).collect();
            fields.push(("surface_replacements".to_owned(), rendered.join(",")));
        }
        Self { fields }
    }

    /// Records that OpenStreetMap features are drawn: `mapping` is the exact `osm.json5` text and
    /// `servers` the data sources, in order.
    #[must_use]
    pub fn with_openstreetmap(mut self, mapping: &[u8], servers: &[String]) -> Self {
        self.fields.push(("osm_mapping".to_owned(), format!("{:016x}:{}", fnv1a64(mapping), mapping.len())));
        self.fields.push(("osm_servers".to_owned(), servers.join(" ")));
        self
    }

    fn render(&self) -> String {
        let mut out = format!("{HEADER}\n");
        for (key, value) in &self.fields {
            out.push_str(&format!("{key}={value}\n"));
        }
        out
    }

    fn parse(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        if lines.next()? != HEADER {
            return None;
        }
        let fields = lines.map(|line| line.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned()))).collect::<Option<Vec<_>>>()?;
        Some(Self { fields })
    }

    /// Fields whose values differ: (name, stored, current).
    pub fn differences(&self, current: &Self) -> Vec<(String, String, String)> {
        let lookup = |fields: &[(String, String)], key: &str| fields.iter().find(|(k, _)| k == key).map_or("<absent>".to_owned(), |(_, v)| v.clone());
        let mut keys: Vec<&String> = self.fields.iter().chain(current.fields.iter()).map(|(k, _)| k).collect();
        keys.sort();
        keys.dedup();
        keys.into_iter()
            .filter_map(|key| {
                let (stored, now) = (lookup(&self.fields, key), lookup(&current.fields, key));
                (stored != now).then(|| (key.clone(), stored, now))
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FingerprintStatus {
    /// No fingerprint existed; the current one was written.
    Created,
    Matched,
}

#[derive(Debug)]
pub enum FingerprintError {
    Mismatch(Vec<(String, String, String)>),
    Corrupt(String),
    Io(std::io::Error),
}

impl std::fmt::Display for FingerprintError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mismatch(differences) => {
                write!(f, "world was generated with different settings:")?;
                for (key, stored, current) in differences {
                    write!(f, " {key}: {stored} -> {current};")?;
                }
                Ok(())
            }
            Self::Corrupt(path) => write!(f, "unreadable generation fingerprint at {path}"),
            Self::Io(error) => write!(f, "generation fingerprint I/O failed: {error}"),
        }
    }
}

impl std::error::Error for FingerprintError {}

/// Checks `directory/FINGERPRINT_FILE_NAME` against `current`, writing it atomically if absent.
/// A mismatch or an unreadable file is an error; nothing is overwritten.
pub fn verify_or_record(directory: &Path, current: &GenerationFingerprint) -> Result<FingerprintStatus, FingerprintError> {
    let path = directory.join(FINGERPRINT_FILE_NAME);
    match fs::read_to_string(&path) {
        Ok(text) => {
            let stored = GenerationFingerprint::parse(&text).ok_or_else(|| FingerprintError::Corrupt(path.display().to_string()))?;
            let differences = stored.differences(current);
            if differences.is_empty() { Ok(FingerprintStatus::Matched) } else { Err(FingerprintError::Mismatch(differences)) }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(directory).map_err(FingerprintError::Io)?;
            let partial = directory.join(format!("{FINGERPRINT_FILE_NAME}.partial-{}", std::process::id()));
            let written = (|| {
                let mut file = fs::File::create(&partial)?;
                file.write_all(current.render().as_bytes())?;
                file.sync_all()?;
                fs::hard_link(&partial, &path)
            })();
            let _ = fs::remove_file(&partial);
            match written {
                Ok(()) => Ok(FingerprintStatus::Created),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => verify_or_record(directory, current),
                Err(error) => Err(FingerprintError::Io(error)),
            }
        }
        Err(error) => Err(FingerprintError::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORLD: WorldHeight = WorldHeight { min_y: -2032, max_y: 2032 };

    #[test]
    fn records_then_matches_then_refuses_changes() {
        let directory = std::env::temp_dir().join(format!("terra-fingerprint-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let settings = GeneratorSettings::default();
        let current = GenerationFingerprint::compute(&settings, WORLD, b"heights", Some(b"koppen"));
        assert_eq!(verify_or_record(&directory, &current).unwrap(), FingerprintStatus::Created);
        assert_eq!(verify_or_record(&directory, &current).unwrap(), FingerprintStatus::Matched);
        let shifted = GenerationFingerprint::compute(&GeneratorSettings { y_offset: -10, ..settings.clone() }, WORLD, b"heights", Some(b"koppen"));
        match verify_or_record(&directory, &shifted) {
            Err(FingerprintError::Mismatch(differences)) => assert_eq!(differences, vec![("y_offset".to_owned(), "0".to_owned(), "-10".to_owned())]),
            other => panic!("expected mismatch, got {other:?}"),
        }
        let edited = GenerationFingerprint::compute(&settings, WORLD, b"heights2", Some(b"koppen"));
        assert!(matches!(verify_or_record(&directory, &edited), Err(FingerprintError::Mismatch(_))));
        assert_eq!(verify_or_record(&directory, &current).unwrap(), FingerprintStatus::Matched, "stored fingerprint left untouched");
        let with_osm = current.clone().with_openstreetmap(b"{}", &["https://osm.example/".to_owned()]);
        match verify_or_record(&directory, &with_osm) {
            Err(FingerprintError::Mismatch(differences)) => {
                let keys: Vec<&str> = differences.iter().map(|(key, stored, _)| {
                    assert_eq!(stored, "<absent>");
                    key.as_str()
                }).collect();
                assert_eq!(keys, vec!["osm_mapping", "osm_servers"]);
            }
            other => panic!("enabling OpenStreetMap must not match a world generated without it: {other:?}"),
        }
        fs::write(directory.join(FINGERPRINT_FILE_NAME), "garbage").unwrap();
        assert!(matches!(verify_or_record(&directory, &current), Err(FingerprintError::Corrupt(_))));
        fs::remove_dir_all(directory).unwrap();
    }
}
