use crate::objects::Hash;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ── Operation types ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Operation {
    Save {
        branch: String,
        commit_hash: Hash,
        previous_head: Option<Hash>,
    },
    Switch {
        from_branch: String,
        to_branch: String,
        previous_head: Option<Hash>,
    },
    Merge {
        from_branch: String,
        onto_branch: String,
        merge_commit: Hash,
        previous_head: Hash,
    },
    BranchCreate {
        name: String,
    },
    BranchDelete {
        name: String,
        deleted_hash: Hash,
    },
    Clean {
        deleted_branches: Vec<(String, Hash)>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OplogEntry {
    /// Unix timestamp in seconds.
    pub timestamp: i64,
    pub operation: Operation,
}

// ── OperationLog ──────────────────────────────────────────────────────────────

/// An append-only log of every repository-mutating operation.
///
/// Entries are written as newline-delimited JSON to `.bait/oplog/ops`.
/// This gives `bait undo` the information needed to reverse any operation.
pub struct OperationLog {
    ops_file: PathBuf,
}

impl OperationLog {
    pub fn new(bait_dir: &Path) -> Self {
        OperationLog {
            ops_file: bait_dir.join("oplog").join("ops"),
        }
    }

    /// Append a new operation to the log.
    pub fn append(&self, op: Operation) -> Result<()> {
        let timestamp = chrono::Utc::now().timestamp();
        let entry = OplogEntry {
            timestamp,
            operation: op,
        };
        let mut line =
            serde_json::to_string(&entry).context("failed to serialise oplog entry")?;
        line.push('\n');

        // Ensure directory exists.
        if let Some(parent) = self.ops_file.parent() {
            std::fs::create_dir_all(parent)
                .context("failed to create oplog directory")?;
        }

        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.ops_file)
            .context("failed to open oplog file")?;
        file.write_all(line.as_bytes())
            .context("failed to append to oplog")?;
        Ok(())
    }

    /// Read all log entries in chronological order.
    pub fn read_all(&self) -> Result<Vec<OplogEntry>> {
        if !self.ops_file.exists() {
            return Ok(vec![]);
        }
        let content =
            std::fs::read_to_string(&self.ops_file).context("failed to read oplog")?;
        let mut entries = Vec::new();
        for (lineno, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let entry: OplogEntry = serde_json::from_str(trimmed)
                .with_context(|| format!("failed to parse oplog line {}", lineno + 1))?;
            entries.push(entry);
        }
        Ok(entries)
    }

    /// Return the most recent entry, or `None` when the log is empty.
    pub fn last(&self) -> Result<Option<OplogEntry>> {
        let all = self.read_all()?;
        Ok(all.into_iter().next_back())
    }

    /// Remove the last entry from the log (called by `bait undo` after reverting).
    pub fn pop_last(&self) -> Result<Option<OplogEntry>> {
        if !self.ops_file.exists() {
            return Ok(None);
        }
        let content = std::fs::read_to_string(&self.ops_file)
            .context("failed to read oplog for undo")?;
        let mut lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            return Ok(None);
        }
        let last_line = lines.pop().unwrap();
        let entry: OplogEntry =
            serde_json::from_str(last_line).context("failed to parse last oplog entry")?;

        // Rewrite the file without the last line.
        let new_content = lines.join("\n") + if lines.is_empty() { "" } else { "\n" };
        std::fs::write(&self.ops_file, new_content)
            .context("failed to rewrite oplog after undo")?;

        Ok(Some(entry))
    }
}
