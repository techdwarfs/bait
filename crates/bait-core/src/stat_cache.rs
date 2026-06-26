use crate::objects::Hash;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedFile {
    pub size: u64,
    pub mtime_ns: u64,
    pub hash: Hash,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StatCache {
    pub entries: BTreeMap<String, CachedFile>,
}

impl StatCache {
    const FILE_NAME: &'static str = "stat-cache";
    const MAGIC: &'static [u8; 6] = b"BSTAT1";

    pub fn load(bait_dir: &Path) -> Result<Self> {
        let path = bait_dir.join(Self::FILE_NAME);
        if !path.exists() {
            return Ok(Self::default());
        }

        let bytes = std::fs::read(&path).context("failed to read stat cache")?;
        if bytes.starts_with(Self::MAGIC) {
            let compressed = &bytes[Self::MAGIC.len()..];
            let raw = zstd::decode_all(std::io::Cursor::new(compressed))
                .context("failed to decompress stat cache")?;
            return bincode::deserialize(&raw).context("failed to deserialize stat cache");
        }

        // Backward compatibility for any early uncompressed format.
        bincode::deserialize(&bytes).context("failed to deserialize stat cache")
    }

    pub fn save(&self, bait_dir: &Path) -> Result<()> {
        let raw = bincode::serialize(self).context("failed to serialize stat cache")?;
        let compressed = zstd::encode_all(std::io::Cursor::new(raw), 3)
            .context("failed to compress stat cache")?;

        let mut bytes = Vec::with_capacity(Self::MAGIC.len() + compressed.len());
        bytes.extend_from_slice(Self::MAGIC);
        bytes.extend_from_slice(&compressed);

        std::fs::write(bait_dir.join(Self::FILE_NAME), bytes)
            .context("failed to write stat cache")?;
        Ok(())
    }

    pub fn retain_seen(&mut self, seen: &BTreeSet<String>) {
        self.entries.retain(|k, _| seen.contains(k));
    }
}

pub fn metadata_signature(meta: &std::fs::Metadata) -> (u64, u64) {
    let size = meta.len();
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    (size, mtime_ns)
}