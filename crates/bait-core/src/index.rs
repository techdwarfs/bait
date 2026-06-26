use crate::objects::Hash;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

// ── IndexEntry ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexEntry {
    /// BLAKE3 hash of the blob that was staged.
    pub hash: Hash,
    /// Whether the file had the executable bit when it was staged.
    pub executable: bool,
}

// ── Index ─────────────────────────────────────────────────────────────────────

/// The staging area: a map from repo-relative file paths to staged blob hashes.
///
/// An empty index means "no explicit staging" — `bait save` will then snapshot
/// the entire working directory automatically (the easy beginner path).
///
/// Persisted as bincode at `.bait/index`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Index {
    /// Keys are repo-relative paths with forward slashes (`src/main.rs`).
    pub entries: BTreeMap<String, IndexEntry>,
}

impl Index {
    const FILE_NAME: &'static str = "index";
    const MAGIC: &'static [u8; 5] = b"BIDX1";

    /// Load the index from `.bait/index`, or return an empty Index when the file
    /// does not exist yet.
    pub fn load(bait_dir: &Path) -> Result<Self> {
        let path = bait_dir.join(Self::FILE_NAME);
        if !path.exists() {
            return Ok(Index::default());
        }
        let bytes = std::fs::read(&path).context("failed to read index file")?;

        if bytes.starts_with(Self::MAGIC) {
            let compressed = &bytes[Self::MAGIC.len()..];
            let raw = zstd::decode_all(std::io::Cursor::new(compressed))
                .context("failed to decompress index")?;
            return bincode::deserialize(&raw).context("failed to deserialise index");
        }

        // Backward compatibility: old indexes were plain bincode.
        bincode::deserialize(&bytes).context("failed to deserialise index")
    }

    /// Persist the index to `.bait/index`.
    pub fn save(&self, bait_dir: &Path) -> Result<()> {
        let raw = bincode::serialize(self).context("failed to serialise index")?;
        let compressed = zstd::encode_all(std::io::Cursor::new(raw), 3)
            .context("failed to compress index")?;

        let mut bytes = Vec::with_capacity(Self::MAGIC.len() + compressed.len());
        bytes.extend_from_slice(Self::MAGIC);
        bytes.extend_from_slice(&compressed);

        std::fs::write(bait_dir.join(Self::FILE_NAME), bytes)
            .context("failed to write index file")?;
        Ok(())
    }

    /// Stage a single file.
    pub fn add(&mut self, path: String, hash: Hash, executable: bool) {
        self.entries.insert(path, IndexEntry { hash, executable });
    }

    /// Remove a path from the index (un-stage).
    pub fn remove(&mut self, path: &str) {
        self.entries.remove(path);
    }

    /// Clear the entire index (called after a successful `bait save`).
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Return true when there is nothing staged.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
