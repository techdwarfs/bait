use crate::objects::Hash;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// A content-addressed object store backed by the local filesystem.
///
/// Objects live at  `<bait_dir>/objects/<first-2-hex>/<remaining-62-hex>`.
/// Each object is zstd-compressed before writing and decompressed on read.
/// Writes are atomic: content is written to a `.tmp` file and renamed.
pub struct ObjectStore {
    objects_dir: PathBuf,
}

impl ObjectStore {
    pub fn new(bait_dir: &Path) -> Self {
        ObjectStore {
            objects_dir: bait_dir.join("objects"),
        }
    }

    /// Create the `objects/` directory tree.
    pub fn init(&self) -> Result<()> {
        std::fs::create_dir_all(&self.objects_dir)
            .context("failed to create objects directory")?;
        Ok(())
    }

    /// Write raw bytes into the store and return their BLAKE3 hash.
    /// Idempotent: writing the same bytes twice is a no-op (the file already exists).
    pub fn write(&self, data: &[u8]) -> Result<Hash> {
        let hash_bytes = *blake3::hash(data).as_bytes();
        let hash = Hash(hash_bytes);
        let hex = hash.to_hex();

        let shard_dir = self.objects_dir.join(&hex[..2]);
        std::fs::create_dir_all(&shard_dir)
            .context("failed to create object shard directory")?;

        let obj_path = shard_dir.join(&hex[2..]);
        if !obj_path.exists() {
            let level = compression_level_for_size(data.len());
            let compressed =
                zstd::encode_all(data, level).context("zstd compression failed")?;

            // Atomic write: temp file → rename.
            let tmp_path = obj_path.with_extension("tmp");
            std::fs::write(&tmp_path, &compressed)
                .context("failed to write object to temp file")?;
            std::fs::rename(&tmp_path, &obj_path)
                .context("failed to finalise object file (rename)")?;
        }

        Ok(hash)
    }

    /// Read and decompress raw bytes identified by `hash`.
    pub fn read(&self, hash: &Hash) -> Result<Vec<u8>> {
        let hex = hash.to_hex();
        let obj_path = self.objects_dir.join(&hex[..2]).join(&hex[2..]);

        let compressed = std::fs::read(&obj_path)
            .with_context(|| format!("object {} not found", &hex[..8]))?;

        let data = zstd::decode_all(std::io::Cursor::new(compressed))
            .context("zstd decompression failed")?;

        Ok(data)
    }

    /// Check whether an object exists in the store without reading it.
    pub fn exists(&self, hash: &Hash) -> bool {
        let hex = hash.to_hex();
        self.objects_dir.join(&hex[..2]).join(&hex[2..]).exists()
    }
}

fn compression_level_for_size(size: usize) -> i32 {
    if size <= 4 * 1024 {
        1
    } else if size <= 256 * 1024 {
        3
    } else if size <= 2 * 1024 * 1024 {
        6
    } else {
        10
    }
}
