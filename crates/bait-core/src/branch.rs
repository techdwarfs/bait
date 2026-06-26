use crate::objects::Hash;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Manages local branch refs stored as plain text files under
/// `.bait/refs/heads/<branch-name>`.
///
/// The content of each file is the 64-character hex BLAKE3 hash of the
/// tip commit for that branch.
pub struct BranchStore {
    heads_dir: PathBuf,
}

impl BranchStore {
    pub fn new(bait_dir: &Path) -> Self {
        BranchStore {
            heads_dir: bait_dir.join("refs").join("heads"),
        }
    }

    /// Read the tip commit hash for `branch`.  Returns `None` when the branch
    /// exists as a name (e.g. just created) but has no commits yet.
    pub fn read(&self, branch: &str) -> Result<Option<Hash>> {
        let path = self.branch_path(branch);
        if !path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read ref for branch '{}'", branch))?;
        let hex = raw.trim();
        if hex.is_empty() {
            return Ok(None);
        }
        Ok(Some(Hash::from_hex(hex)?))
    }

    /// Write (or update) the tip commit hash for `branch`.
    pub fn write(&self, branch: &str, hash: &Hash) -> Result<()> {
        std::fs::create_dir_all(&self.heads_dir)
            .context("failed to ensure refs/heads directory exists")?;
        let path = self.branch_path(branch);
        std::fs::write(&path, format!("{}\n", hash.to_hex()))
            .with_context(|| format!("failed to write ref for branch '{}'", branch))?;
        Ok(())
    }

    /// Delete a branch ref.  Returns an error when trying to delete a branch
    /// that does not exist.
    pub fn delete(&self, branch: &str) -> Result<()> {
        let path = self.branch_path(branch);
        if !path.exists() {
            anyhow::bail!("branch '{}' does not exist", branch);
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("failed to delete branch '{}'", branch))?;
        Ok(())
    }

    /// List all local branch names, sorted alphabetically.
    pub fn list(&self) -> Result<Vec<String>> {
        if !self.heads_dir.exists() {
            return Ok(vec![]);
        }
        let mut names = Vec::new();
        for entry in std::fs::read_dir(&self.heads_dir)
            .context("failed to read refs/heads")?
        {
            let entry = entry?;
            if entry.path().is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    /// Return `true` when `branch` exists.
    pub fn exists(&self, branch: &str) -> bool {
        self.branch_path(branch).exists()
    }

    fn branch_path(&self, branch: &str) -> PathBuf {
        self.heads_dir.join(branch)
    }
}
