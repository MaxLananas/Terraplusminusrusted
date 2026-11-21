//! Whether a chunk has been saved, read from the location table of its Anvil region file
//! (`r.<x>.<z>.mca`: 1024 big-endian entries of a 3-byte sector offset and a 1-byte sector
//! count). Only the 4 KiB header is read; a cached header is re-read when the file's
//! modification time or length changes, or once it is older than the freshness window (file
//! times can be too coarse to show a rewrite).

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

const HEADER_BYTES: usize = 4096;

struct CachedHeader {
    modified: Option<SystemTime>,
    length: u64,
    read_at: Instant,
    present: Box<[bool; 1024]>,
}

pub struct RegionIndex {
    directory: PathBuf,
    capacity: usize,
    freshness: Duration,
    cache: Mutex<HashMap<(i32, i32), CachedHeader>>,
}

impl RegionIndex {
    pub fn new(directory: impl Into<PathBuf>, capacity: usize, freshness: Duration) -> Self {
        Self { directory: directory.into(), capacity: capacity.max(1), freshness, cache: Mutex::new(HashMap::new()) }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn chunk_saved(&self, chunk_x: i32, chunk_z: i32) -> std::io::Result<bool> {
        let region = (chunk_x >> 5, chunk_z >> 5);
        let path = self.directory.join(format!("r.{}.{}.mca", region.0, region.1));
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let slot = ((chunk_x & 31) + (chunk_z & 31) * 32) as usize;
        let modified = metadata.modified().ok();
        let mut cache = self.cache.lock().map_err(|_| std::io::Error::other("region index lock poisoned"))?;
        if let Some(cached) = cache.get(&region)
            && cached.modified == modified
            && cached.length == metadata.len()
            && cached.read_at.elapsed() < self.freshness
        {
            return Ok(cached.present[slot]);
        }
        let present = read_header(&path)?;
        let answer = present[slot];
        if cache.len() >= self.capacity {
            cache.clear();
        }
        cache.insert(region, CachedHeader { modified, length: metadata.len(), read_at: Instant::now(), present });
        Ok(answer)
    }
}

fn read_header(path: &Path) -> std::io::Result<Box<[bool; 1024]>> {
    let mut header = vec![0u8; HEADER_BYTES];
    let mut file = std::fs::File::open(path)?;
    let mut filled = 0;
    while filled < HEADER_BYTES {
        match file.read(&mut header[filled..])? {
            0 => break,
            read => filled += read,
        }
    }
    let mut present = Box::new([false; 1024]);
    if filled < HEADER_BYTES {
        return Ok(present);
    }
    for (slot, entry) in header.chunks_exact(4).enumerate() {
        let offset = u32::from_be_bytes([0, entry[0], entry[1], entry[2]]);
        present[slot] = offset >= 2 && entry[3] > 0;
    }
    Ok(present)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_region(directory: &Path, region: (i32, i32), slots: &[usize]) {
        let mut bytes = vec![0u8; HEADER_BYTES * 2];
        for (index, slot) in slots.iter().enumerate() {
            let offset = 2 + index as u32;
            bytes[slot * 4..slot * 4 + 3].copy_from_slice(&offset.to_be_bytes()[1..]);
            bytes[slot * 4 + 3] = 1;
        }
        bytes.resize(HEADER_BYTES * 2 + slots.len() * 4096, 0);
        std::fs::write(directory.join(format!("r.{}.{}.mca", region.0, region.1)), bytes).unwrap();
    }

    #[test]
    fn reads_saved_chunks_including_negative_coordinates() {
        let directory = std::env::temp_dir().join(format!("terra-region-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        write_region(&directory, (-1, -2), &[31 + 31 * 32, 0]);
        let index = RegionIndex::new(&directory, 8, Duration::ZERO);
        assert!(index.chunk_saved(-1, -33).unwrap());
        assert!(index.chunk_saved(-32, -64).unwrap());
        assert!(!index.chunk_saved(-2, -33).unwrap());
        assert!(!index.chunk_saved(0, 0).unwrap());
        std::fs::write(directory.join("r.0.0.mca"), [0u8; 100]).unwrap();
        assert!(!index.chunk_saved(0, 0).unwrap());
        write_region(&directory, (-1, -2), &[0, 1]);
        assert!(!index.chunk_saved(-1, -33).unwrap(), "a rewritten region file is re-read");
        assert!(index.chunk_saved(-31, -64).unwrap());
        let cached = RegionIndex::new(&directory, 8, Duration::from_secs(3600));
        assert!(cached.chunk_saved(-31, -64).unwrap());
        std::fs::remove_file(directory.join("r.-1.-2.mca")).unwrap();
        assert!(!cached.chunk_saved(-31, -64).unwrap(), "a deleted region file is never served from cache");
    }
}
