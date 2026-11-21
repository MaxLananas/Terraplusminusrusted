//! Size-bounded on-disk tile cache. Entries are published atomically (write, fsync, rename),
//! keyed by a versioned hash of the URL, carry the URL and a CRC so collisions and torn or
//! corrupt files are detected, and are evicted least-recently-used beyond the byte budget.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};
use terra_elevation::{ElevationError, FetchOutcome, TileFetcher};

/// Bump when the entry layout or the meaning of cached bytes changes.
pub const CACHE_FORMAT_VERSION: &str = "v1";
const MAGIC: &[u8; 6] = b"TPMC1\n";
const ENTRY_EXTENSION: &str = "tile";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskCacheLimits {
    pub max_bytes: u64,
    /// How long a cached "missing" (HTTP 404) answer stays valid.
    pub missing_ttl: Duration,
}

impl Default for DiskCacheLimits {
    fn default() -> Self {
        Self { max_bytes: 2 << 30, missing_ttl: Duration::from_secs(24 * 3600) }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DiskCacheStatistics {
    pub entries: usize,
    pub bytes: u64,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub corrupt_entries: u64,
    pub write_failures: u64,
}

#[derive(Default)]
struct Index {
    entries: HashMap<String, (u64, u64)>,
    recency: BTreeMap<u64, String>,
    clock: u64,
    bytes: u64,
    statistics: DiskCacheStatistics,
}

impl Index {
    fn touch(&mut self, key: &str, size: u64) {
        self.clock += 1;
        if let Some((old_size, old_tick)) = self.entries.insert(key.to_owned(), (size, self.clock)) {
            self.recency.remove(&old_tick);
            self.bytes -= old_size;
        }
        self.recency.insert(self.clock, key.to_owned());
        self.bytes += size;
    }

    fn forget(&mut self, key: &str) {
        if let Some((size, tick)) = self.entries.remove(key) {
            self.recency.remove(&tick);
            self.bytes -= size;
        }
    }
}

pub struct DiskCache {
    directory: PathBuf,
    limits: DiskCacheLimits,
    index: Mutex<Index>,
    temp_counter: AtomicU64,
}

fn fnv1a(bytes: &[u8], basis: u64) -> u64 {
    bytes.iter().fold(basis, |hash, &byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

fn cache_key(url: &str) -> String {
    format!("{:016x}{:016x}", fnv1a(url.as_bytes(), 0xcbf2_9ce4_8422_2325), fnv1a(url.as_bytes(), 0x6c62_272e_07bb_0142))
}

fn encode_entry(url: &str, outcome: &FetchOutcome) -> Vec<u8> {
    let (kind, body): (u8, &[u8]) = match outcome {
        FetchOutcome::Found(body) => (1, body),
        FetchOutcome::Missing => (0, &[]),
    };
    let mut out = Vec::with_capacity(MAGIC.len() + 17 + url.len() + body.len());
    out.extend_from_slice(MAGIC);
    out.push(kind);
    out.extend_from_slice(&(url.len() as u32).to_le_bytes());
    out.extend_from_slice(url.as_bytes());
    out.extend_from_slice(&(body.len() as u64).to_le_bytes());
    out.extend_from_slice(body);
    let crc = terra_codec::crc32(&out);
    out.extend_from_slice(&crc.to_le_bytes());
    out
}

fn decode_entry(url: &str, bytes: &[u8]) -> Option<FetchOutcome> {
    let (content, crc) = bytes.split_at_checked(bytes.len().checked_sub(4)?)?;
    if terra_codec::crc32(content).to_le_bytes() != crc || !content.starts_with(MAGIC) {
        return None;
    }
    let rest = &content[MAGIC.len()..];
    let (&kind, rest) = rest.split_first()?;
    let (url_length, rest) = rest.split_at_checked(4)?;
    let (stored_url, rest) = rest.split_at_checked(u32::from_le_bytes(url_length.try_into().ok()?) as usize)?;
    let (body_length, body) = rest.split_at_checked(8)?;
    if stored_url != url.as_bytes() || u64::from_le_bytes(body_length.try_into().ok()?) != body.len() as u64 {
        return None;
    }
    match kind {
        1 => Some(FetchOutcome::Found(body.to_vec())),
        0 if body.is_empty() => Some(FetchOutcome::Missing),
        _ => None,
    }
}

impl DiskCache {
    /// Opens (creating if needed) `root/<CACHE_FORMAT_VERSION>`, discards leftover partial writes
    /// and indexes existing entries by modification time.
    pub fn open(root: &Path, limits: DiskCacheLimits) -> std::io::Result<Self> {
        let directory = root.join(CACHE_FORMAT_VERSION);
        fs::create_dir_all(directory.join("partial"))?;
        for leftover in fs::read_dir(directory.join("partial"))? {
            fs::remove_file(leftover?.path())?;
        }
        let mut found = Vec::new();
        for shard in fs::read_dir(&directory)? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() || shard.file_name() == "partial" {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                let entry = entry?;
                let path = entry.path();
                let Some(key) = path.file_stem().and_then(|stem| stem.to_str()).filter(|key| key.len() == 32) else { continue };
                if path.extension().and_then(|ext| ext.to_str()) != Some(ENTRY_EXTENSION) {
                    continue;
                }
                let metadata = entry.metadata()?;
                found.push((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), key.to_owned(), metadata.len()));
            }
        }
        found.sort();
        let mut index = Index::default();
        for (_, key, size) in found {
            index.touch(&key, size);
        }
        let cache = Self { directory, limits, index: Mutex::new(index), temp_counter: AtomicU64::new(0) };
        cache.evict_over_budget();
        Ok(cache)
    }

    fn entry_path(&self, key: &str) -> PathBuf {
        self.directory.join(&key[..2]).join(format!("{key}.{ENTRY_EXTENSION}"))
    }

    pub fn statistics(&self) -> DiskCacheStatistics {
        let index = self.index.lock().unwrap_or_else(PoisonError::into_inner);
        DiskCacheStatistics { entries: index.entries.len(), bytes: index.bytes, ..index.statistics }
    }

    pub fn get(&self, url: &str) -> Option<FetchOutcome> {
        let key = cache_key(url);
        let path = self.entry_path(&key);
        let known = self.index.lock().unwrap_or_else(PoisonError::into_inner).entries.contains_key(&key);
        let decoded = if known { fs::read(&path).ok().map(|bytes| (decode_entry(url, &bytes), bytes.len() as u64)) } else { None };
        let mut index = self.index.lock().unwrap_or_else(PoisonError::into_inner);
        match decoded {
            Some((Some(FetchOutcome::Missing), _)) if self.missing_expired(&path) => {
                index.forget(&key);
                index.statistics.misses += 1;
                drop(index);
                let _ = fs::remove_file(&path);
                None
            }
            Some((Some(outcome), size)) => {
                index.touch(&key, size);
                index.statistics.hits += 1;
                Some(outcome)
            }
            Some((None, _)) => {
                index.forget(&key);
                index.statistics.corrupt_entries += 1;
                index.statistics.misses += 1;
                drop(index);
                let _ = fs::remove_file(&path);
                None
            }
            None => {
                if known {
                    index.forget(&key);
                }
                index.statistics.misses += 1;
                None
            }
        }
    }

    fn missing_expired(&self, path: &Path) -> bool {
        let modified = fs::metadata(path).and_then(|metadata| metadata.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
        SystemTime::now().duration_since(modified).unwrap_or_default() > self.limits.missing_ttl
    }

    pub fn put(&self, url: &str, outcome: &FetchOutcome) -> std::io::Result<()> {
        let key = cache_key(url);
        let bytes = encode_entry(url, outcome);
        if bytes.len() as u64 > self.limits.max_bytes {
            return Ok(());
        }
        let partial = self.directory.join("partial").join(format!("{key}.{}.{}", std::process::id(), self.temp_counter.fetch_add(1, Ordering::Relaxed)));
        let result = (|| {
            let mut file = fs::File::create(&partial)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            let target = self.entry_path(&key);
            fs::create_dir_all(target.parent().unwrap_or(&self.directory))?;
            fs::rename(&partial, &target)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&partial);
            self.index.lock().unwrap_or_else(PoisonError::into_inner).statistics.write_failures += 1;
            return result;
        }
        self.index.lock().unwrap_or_else(PoisonError::into_inner).touch(&key, bytes.len() as u64);
        self.evict_over_budget();
        Ok(())
    }

    fn evict_over_budget(&self) {
        loop {
            let victim = {
                let mut index = self.index.lock().unwrap_or_else(PoisonError::into_inner);
                if index.bytes <= self.limits.max_bytes {
                    return;
                }
                let Some((_, key)) = index.recency.first_key_value().map(|(tick, key)| (*tick, key.clone())) else { return };
                index.forget(&key);
                index.statistics.evictions += 1;
                key
            };
            let _ = fs::remove_file(self.entry_path(&victim));
        }
    }
}

pub struct DiskCachedFetcher<F> {
    cache: DiskCache,
    inner: F,
}

impl<F: TileFetcher> DiskCachedFetcher<F> {
    pub fn new(cache: DiskCache, inner: F) -> Self {
        Self { cache, inner }
    }

    pub fn cache(&self) -> &DiskCache {
        &self.cache
    }
}

impl<F: TileFetcher> TileFetcher for DiskCachedFetcher<F> {
    fn fetch(&self, url: &str) -> Result<FetchOutcome, ElevationError> {
        if let Some(outcome) = self.cache.get(url) {
            return Ok(outcome);
        }
        let outcome = self.inner.fetch(url)?;
        let _ = self.cache.put(url, &outcome);
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("terra-disk-cache-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn round_trips_and_survives_reopen() {
        let root = scratch("reopen");
        let cache = DiskCache::open(&root, DiskCacheLimits::default()).unwrap();
        cache.put("http://a/1.png", &FetchOutcome::Found(vec![1, 2, 3])).unwrap();
        cache.put("http://a/2.png", &FetchOutcome::Missing).unwrap();
        drop(cache);
        let cache = DiskCache::open(&root, DiskCacheLimits::default()).unwrap();
        assert_eq!(cache.get("http://a/1.png"), Some(FetchOutcome::Found(vec![1, 2, 3])));
        assert_eq!(cache.get("http://a/2.png"), Some(FetchOutcome::Missing));
        assert_eq!(cache.get("http://a/3.png"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn detects_corruption_and_discards_partial_writes() {
        let root = scratch("corrupt");
        let cache = DiskCache::open(&root, DiskCacheLimits::default()).unwrap();
        cache.put("http://a/1.png", &FetchOutcome::Found(vec![9; 100])).unwrap();
        let path = cache.entry_path(&cache_key("http://a/1.png"));
        let mut bytes = fs::read(&path).unwrap();
        bytes[40] ^= 0xFF;
        fs::write(&path, bytes).unwrap();
        assert_eq!(cache.get("http://a/1.png"), None);
        assert_eq!(cache.statistics().corrupt_entries, 1);
        assert!(!path.exists());
        fs::write(root.join(CACHE_FORMAT_VERSION).join("partial").join("torn"), b"x").unwrap();
        drop(cache);
        DiskCache::open(&root, DiskCacheLimits::default()).unwrap();
        assert_eq!(fs::read_dir(root.join(CACHE_FORMAT_VERSION).join("partial")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn evicts_least_recently_used_beyond_budget() {
        let root = scratch("evict");
        let limits = DiskCacheLimits { max_bytes: 2500, ..DiskCacheLimits::default() };
        let cache = DiskCache::open(&root, limits).unwrap();
        for i in 0..3 {
            cache.put(&format!("http://a/{i}"), &FetchOutcome::Found(vec![0; 1000])).unwrap();
            if i == 1 {
                assert!(cache.get("http://a/0").is_some());
            }
        }
        assert!(cache.get("http://a/0").is_some());
        assert!(cache.get("http://a/1").is_none());
        let statistics = cache.statistics();
        assert!(statistics.bytes <= 2500 && statistics.evictions == 1, "{statistics:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn url_mismatch_is_a_miss() {
        let entry = encode_entry("http://a/1", &FetchOutcome::Found(vec![1]));
        assert_eq!(decode_entry("http://a/2", &entry), None);
        assert_eq!(decode_entry("http://a/1", &entry), Some(FetchOutcome::Found(vec![1])));
        assert_eq!(decode_entry("http://a/1", &entry[..entry.len() - 1]), None);
    }
}
