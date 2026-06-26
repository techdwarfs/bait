use serde::{Deserialize, Serialize};
use std::fmt;

// ── Hash ─────────────────────────────────────────────────────────────────────

/// A BLAKE3 content-address: 32 raw bytes, displayed as 64 lowercase hex chars.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Hash(pub [u8; 32]);

impl Hash {
    /// Encode as 64-character lowercase hex string.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{:02x}", b)).collect()
    }

    /// Decode from a 64-character hex string.
    pub fn from_hex(s: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            s.len() == 64,
            "hash must be 64 hex characters, got {}",
            s.len()
        );
        let mut bytes = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hex_byte = std::str::from_utf8(chunk)
                .map_err(|_| anyhow::anyhow!("invalid utf-8 in hash"))?;
            bytes[i] = u8::from_str_radix(hex_byte, 16)
                .map_err(|_| anyhow::anyhow!("invalid hex character in hash: {}", hex_byte))?;
        }
        Ok(Hash(bytes))
    }

    /// Returns the first 8 hex characters (16 bits) — suitable for short display.
    pub fn short(&self) -> String {
        self.to_hex()[..8].to_string()
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Hash({})", &self.to_hex()[..8])
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

// ── Blob ─────────────────────────────────────────────────────────────────────

/// Raw file contents stored in the object store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blob {
    pub data: Vec<u8>,
}

// ── Tree ─────────────────────────────────────────────────────────────────────

/// One entry in a Tree: either a file (blob) or sub-directory (tree).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeEntry {
    /// File or directory name — a single path component, never a full path.
    pub name: String,
    /// BLAKE3 hash of the child blob or tree.
    pub hash: Hash,
    /// True when this entry is itself a Tree (directory).
    pub is_dir: bool,
    /// True when the file has the executable bit set (Unix only; ignored on Windows).
    pub executable: bool,
}

/// A sorted snapshot of one directory level.
/// Entries are sorted by name for deterministic content-addressing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tree {
    pub entries: Vec<TreeEntry>,
}

impl Tree {
    /// Create a Tree, sorting entries by name.
    pub fn new(mut entries: Vec<TreeEntry>) -> Self {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Tree { entries }
    }

    /// Empty tree (for the initial commit before any files are added).
    pub fn empty() -> Self {
        Tree { entries: vec![] }
    }
}

// ── Commit ───────────────────────────────────────────────────────────────────

/// A single point in the version history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    /// BLAKE3 hash of the root Tree snapshot for this commit.
    pub tree: Hash,

    /// Parent commit hashes.
    /// - Empty   → root commit (first ever commit)
    /// - 1 entry → ordinary commit
    /// - 2 entries → merge commit
    pub parents: Vec<Hash>,

    /// The author's display name.
    /// Displayed as "rushi" (ऋषि — Sanskrit for sage/seer) in the CLI.
    pub rushi: String,

    /// The author's email address.
    pub email: String,

    /// Optional reviewer name.
    /// Displayed as "narada" (नारद — divine messenger) in the CLI.
    pub narada: Option<String>,

    /// Optional reviewer email.
    pub narada_email: Option<String>,

    /// Unix timestamp in seconds (UTC).
    pub timestamp: i64,

    /// The commit message provided with `bait save`.
    pub message: String,

    /// Set to true when the commit was saved while merge conflicts were present.
    /// Conflicts are recorded in the tree with conflict markers rather than
    /// blocking the save — callers can resolve and re-save at their leisure.
    pub has_conflicts: bool,
}
