use crate::{
    branch::BranchStore,
    ignore::IgnoreRules,
    index::Index,
    objects::{Blob, Commit, Hash, Tree, TreeEntry},
    oplog::OperationLog,
    stat_cache::{metadata_signature, CachedFile, StatCache},
    store::ObjectStore,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct CoreConfig {
    pub default_branch: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserConfig {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RepoConfig {
    pub core: CoreConfig,
    pub user: UserConfig,
}

impl Default for RepoConfig {
    fn default() -> Self {
        let name = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".to_string());
        RepoConfig {
            core: CoreConfig {
                default_branch: "main".to_string(),
            },
            user: UserConfig {
                name,
                email: String::new(),
            },
        }
    }
}

// ── Status ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone)]
pub struct StatusEntry {
    pub path: String,
    pub kind: StatusKind,
}

#[derive(Debug, Default)]
pub struct WorkingStatus {
    /// Files staged in the index (will be included in next `bait save`).
    pub staged: Vec<StatusEntry>,
    /// Files changed in the working copy but not yet staged.
    pub unstaged: Vec<StatusEntry>,
    /// Files in the working copy that have never been tracked.
    pub untracked: Vec<String>,
}

// ── Repository ────────────────────────────────────────────────────────────────

pub struct Repository {
    /// Root of the working tree (the folder that contains `.bait/`).
    pub workdir: PathBuf,
    /// Path to the `.bait/` directory.
    pub bait_dir: PathBuf,

    pub store: ObjectStore,
    pub branches: BranchStore,
    pub oplog: OperationLog,
    pub config: RepoConfig,
}

#[derive(Debug, Serialize, Deserialize)]
struct HeadMapCache {
    commit: Hash,
    map: BTreeMap<String, Hash>,
}

impl Repository {
    // ─── Initialisation ──────────────────────────────────────────────────────

    /// Create a new, empty repository at `path`.
    pub fn init(path: &Path) -> Result<Self> {
        let workdir = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let bait_dir = workdir.join(".bait");

        if bait_dir.exists() {
            bail!("repository already exists at {}", bait_dir.display());
        }

        // Create directory structure.
        for sub in &["objects", "refs/heads", "refs/remotes", "oplog", "hooks"] {
            std::fs::create_dir_all(bait_dir.join(sub))
                .with_context(|| format!("failed to create .bait/{}", sub))?;
        }

        let config = RepoConfig::default();
        let config_toml = toml::to_string_pretty(&config)
            .context("failed to serialise default config")?;
        std::fs::write(bait_dir.join("config"), config_toml)
            .context("failed to write config")?;

        // HEAD points to the default branch (which doesn't exist yet — that's fine).
        std::fs::write(
            bait_dir.join("HEAD"),
            format!("ref: refs/heads/{}\n", config.core.default_branch),
        )
        .context("failed to write HEAD")?;

        let store = ObjectStore::new(&bait_dir);
        store.init()?;

        let branches = BranchStore::new(&bait_dir);
        let oplog = OperationLog::new(&bait_dir);

        Ok(Repository {
            workdir,
            bait_dir,
            store,
            branches,
            oplog,
            config,
        })
    }

    /// Open an existing repository, walking up from `start` until `.bait/` is found.
    pub fn open(start: &Path) -> Result<Self> {
        let bait_dir = Self::find_bait_dir(start)
            .context("not inside a bait repository (no .bait/ found)")?;
        let workdir = bait_dir.parent().unwrap().to_path_buf();

        let config_raw = std::fs::read_to_string(bait_dir.join("config"))
            .context("failed to read .bait/config")?;
        let config: RepoConfig =
            toml::from_str(&config_raw).context("failed to parse .bait/config")?;

        let store = ObjectStore::new(&bait_dir);
        let branches = BranchStore::new(&bait_dir);
        let oplog = OperationLog::new(&bait_dir);

        Ok(Repository {
            workdir,
            bait_dir,
            store,
            branches,
            oplog,
            config,
        })
    }

    /// Walk up the directory tree from `start` to find a `.bait/` directory.
    fn find_bait_dir(start: &Path) -> Option<PathBuf> {
        let mut current = start.canonicalize().ok()?;
        loop {
            let candidate = current.join(".bait");
            if candidate.is_dir() {
                return Some(candidate);
            }
            if !current.pop() {
                return None;
            }
        }
    }

    // ─── HEAD / branch helpers ────────────────────────────────────────────────

    /// Read the raw contents of `.bait/HEAD`.
    fn read_head_raw(&self) -> Result<String> {
        let raw = std::fs::read_to_string(self.bait_dir.join("HEAD"))
            .context("failed to read HEAD")?;
        Ok(raw.trim().to_string())
    }

    /// Return the name of the currently checked-out branch.
    /// Returns an error when HEAD is detached (bare hash).
    pub fn current_branch(&self) -> Result<String> {
        let raw = self.read_head_raw()?;
        if let Some(branch) = raw.strip_prefix("ref: refs/heads/") {
            Ok(branch.to_string())
        } else {
            bail!("HEAD is detached (not on a branch)")
        }
    }

    /// Return the current HEAD commit hash, or `None` when the branch has no commits.
    pub fn head_commit(&self) -> Result<Option<Hash>> {
        let raw = self.read_head_raw()?;
        if let Some(branch) = raw.strip_prefix("ref: refs/heads/") {
            self.branches.read(branch)
        } else {
            // Detached HEAD — raw is the hash itself.
            Ok(Some(Hash::from_hex(&raw)?))
        }
    }

    /// Advance the current branch pointer to `hash` (after a save/merge).
    pub fn set_head_commit(&self, hash: &Hash) -> Result<()> {
        let branch = self.current_branch()?;
        self.branches.write(&branch, hash)
    }

    // ─── Object helpers ───────────────────────────────────────────────────────

    pub fn write_commit(&self, commit: &Commit) -> Result<Hash> {
        let bytes = bincode::serialize(commit).context("failed to serialise commit")?;
        self.store.write(&bytes)
    }

    pub fn read_commit(&self, hash: &Hash) -> Result<Commit> {
        let bytes = self.store.read(hash)?;
        bincode::deserialize(&bytes).context("failed to deserialise commit")
    }

    pub fn write_tree(&self, tree: &Tree) -> Result<Hash> {
        let bytes = bincode::serialize(tree).context("failed to serialise tree")?;
        self.store.write(&bytes)
    }

    pub fn read_tree(&self, hash: &Hash) -> Result<Tree> {
        let bytes = self.store.read(hash)?;
        bincode::deserialize(&bytes).context("failed to deserialise tree")
    }

    pub fn write_blob(&self, data: &[u8]) -> Result<Hash> {
        let blob = Blob { data: data.to_vec() };
        let bytes = bincode::serialize(&blob).context("failed to serialise blob")?;
        self.store.write(&bytes)
    }

    pub fn read_blob(&self, hash: &Hash) -> Result<Vec<u8>> {
        let bytes = self.store.read(hash)?;
        let blob: Blob =
            bincode::deserialize(&bytes).context("failed to deserialise blob")?;
        Ok(blob.data)
    }

    // ─── Snapshot ─────────────────────────────────────────────────────────────

    /// Build a Tree object from the working directory, respecting `.baitignore`.
    ///
    /// If `index` is `Some` and non-empty, only staged paths are included.
    /// If `index` is `None` or empty, the entire working tree is snapshotted.
    pub fn snapshot_tree(&self, index: Option<&Index>) -> Result<Hash> {
        let ignore = IgnoreRules::load(&self.workdir, &self.bait_dir)?;

        // If the index has staged entries, use only those; otherwise snapshot all.
        let use_index = index.map_or(false, |i| !i.entries.is_empty());

        if use_index {
            self.snapshot_from_index(index.unwrap(), &ignore)
        } else {
            self.snapshot_dir(&self.workdir, &ignore)
        }
    }

    /// Build a tree from the staged index entries.
    fn snapshot_from_index(&self, index: &Index, ignore: &IgnoreRules) -> Result<Hash> {
        // Group entries by directory hierarchy.
        let mut dir_map: BTreeMap<String, Vec<TreeEntry>> = BTreeMap::new();

        for (path, entry) in &index.entries {
            if ignore.is_ignored(Path::new(path)) {
                continue;
            }

            let (dir_part, file_name) = split_path(path);

            dir_map
                .entry(dir_part)
                .or_default()
                .push(TreeEntry {
                    name: file_name,
                    hash: entry.hash.clone(),
                    is_dir: false,
                    executable: entry.executable,
                });
        }

        self.assemble_tree_from_dir_map(dir_map)
    }

    /// Recursively snapshot a directory into a Tree.
    fn snapshot_dir(&self, dir: &Path, ignore: &IgnoreRules) -> Result<Hash> {
        let mut entries = Vec::new();

        for entry in std::fs::read_dir(dir).with_context(|| {
            format!("failed to read directory: {}", dir.display())
        })? {
            let entry = entry?;
            let path = entry.path();

            // Always skip the .bait directory itself.
            if path == self.bait_dir {
                continue;
            }

            let rel_path = path
                .strip_prefix(&self.workdir)
                .unwrap_or(&path)
                .to_path_buf();

            if ignore.is_ignored(&rel_path) {
                continue;
            }

            let name = entry
                .file_name()
                .to_string_lossy()
                .into_owned();

            let metadata = entry.metadata()?;

            if metadata.is_dir() {
                let child_hash = self.snapshot_dir(&path, ignore)?;
                entries.push(TreeEntry {
                    name,
                    hash: child_hash,
                    is_dir: true,
                    executable: false,
                });
            } else if metadata.is_file() {
                let data = std::fs::read(&path)
                    .with_context(|| format!("failed to read file: {}", path.display()))?;
                let blob_hash = self.write_blob(&data)?;
                let executable = is_executable(&path);
                entries.push(TreeEntry {
                    name,
                    hash: blob_hash,
                    is_dir: false,
                    executable,
                });
            }
        }

        let tree = Tree::new(entries);
        self.write_tree(&tree)
    }

    /// Assemble nested Tree objects from a flat dir_map produced by `snapshot_from_index`.
    fn assemble_tree_from_dir_map(
        &self,
        dir_map: BTreeMap<String, Vec<TreeEntry>>,
    ) -> Result<Hash> {
        // Build the root entries.
        let root_entries = dir_map
            .get("")
            .cloned()
            .unwrap_or_default();

        let mut top_entries = root_entries;

        // For each sub-directory that has entries, build a sub-tree.
        for (dir, entries) in &dir_map {
            if dir.is_empty() {
                continue;
            }
            // Only handle one level deep for simplicity in staged mode.
            // Full recursive staging is handled by snapshot_dir.
            let sub_tree = Tree::new(entries.clone());
            let sub_hash = self.write_tree(&sub_tree)?;
            let dir_name = dir.split('/').next_back().unwrap_or(dir).to_string();
            top_entries.push(TreeEntry {
                name: dir_name,
                hash: sub_hash,
                is_dir: true,
                executable: false,
            });
        }

        let root_tree = Tree::new(top_entries);
        self.write_tree(&root_tree)
    }

    // ─── Checkout ─────────────────────────────────────────────────────────────

    /// Restore the working directory to the state recorded in `commit_hash`.
    pub fn checkout_commit(&self, commit_hash: &Hash) -> Result<()> {
        let commit = self.read_commit(commit_hash)?;
        self.checkout_tree(&commit.tree, &self.workdir.clone())
    }

    fn checkout_tree(&self, tree_hash: &Hash, dir: &Path) -> Result<()> {
        let tree = self.read_tree(tree_hash)?;

        // Collect names from the new tree.
        let new_names: std::collections::HashSet<&str> =
            tree.entries.iter().map(|e| e.name.as_str()).collect();

        // Remove files/dirs in the directory that are no longer in the tree.
        if dir.exists() {
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                // Never remove .bait.
                if dir == self.workdir && name == ".bait" {
                    continue;
                }
                if !new_names.contains(name.as_str()) {
                    let path = entry.path();
                    if path.is_dir() {
                        std::fs::remove_dir_all(&path)?;
                    } else {
                        std::fs::remove_file(&path)?;
                    }
                }
            }
        } else {
            std::fs::create_dir_all(dir)?;
        }

        // Write each entry.
        for te in &tree.entries {
            let target = dir.join(&te.name);
            if te.is_dir {
                self.checkout_tree(&te.hash, &target)?;
            } else {
                let data = self.read_blob(&te.hash)?;
                std::fs::write(&target, &data)?;
                #[cfg(unix)]
                if te.executable {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = std::fs::metadata(&target)?.permissions();
                    perms.set_mode(perms.mode() | 0o111);
                    std::fs::set_permissions(&target, perms)?;
                }
            }
        }

        Ok(())
    }

    // ─── Status ───────────────────────────────────────────────────────────────

    /// Compute the difference between the working copy, the staged index, and HEAD.
    pub fn working_status(&self, index: &Index) -> Result<WorkingStatus> {
        let ignore = IgnoreRules::load(&self.workdir, &self.bait_dir)?;

        // Build a flat map of head_path → blob_hash from the HEAD commit tree.
        let head_map = match self.head_commit()? {
            Some(h) => {
                if let Some(cache) = self.load_head_map_cache() {
                    if cache.commit == h {
                        cache.map
                    } else {
                        let commit = self.read_commit(&h)?;
                        let map = self.flatten_tree(&commit.tree, "")?;
                        let _ = self.save_head_map_cache(&HeadMapCache {
                            commit: h,
                            map: map.clone(),
                        });
                        map
                    }
                } else {
                    let commit = self.read_commit(&h)?;
                    let map = self.flatten_tree(&commit.tree, "")?;
                    let _ = self.save_head_map_cache(&HeadMapCache {
                        commit: h,
                        map: map.clone(),
                    });
                    map
                }
            }
            None => BTreeMap::new(),
        };

        // Build a flat map of working copy path → current content hash.
        let working_map = self.flatten_workdir(&ignore)?;

        let staged_map = &index.entries;

        let mut status = WorkingStatus::default();

        // All paths ever seen.
        let mut all_paths: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        all_paths.extend(head_map.keys().cloned());
        all_paths.extend(working_map.keys().cloned());
        all_paths.extend(staged_map.keys().cloned());

        for path in &all_paths {
            let in_head = head_map.get(path);
            let in_working = working_map.get(path);
            let in_index = staged_map.get(path);

            // Staged status.
            if let Some(idx_entry) = in_index {
                let kind = match in_head {
                    None => StatusKind::Added,
                    Some(h) if *h != idx_entry.hash => StatusKind::Modified,
                    _ => continue,
                };
                status.staged.push(StatusEntry { path: path.clone(), kind });
            }

            // Unstaged / untracked status.
            if in_index.is_none() {
                match (in_head, in_working) {
                    (None, Some(_)) => {
                        status.untracked.push(path.clone());
                    }
                    (Some(head_h), Some(work_h)) if head_h != work_h => {
                        status.unstaged.push(StatusEntry {
                            path: path.clone(),
                            kind: StatusKind::Modified,
                        });
                    }
                    (Some(_), None) => {
                        status.unstaged.push(StatusEntry {
                            path: path.clone(),
                            kind: StatusKind::Deleted,
                        });
                    }
                    _ => {}
                }
            }
        }

        Ok(status)
    }

    /// Flatten a Tree into a map of `repo-relative-path → blob Hash`.
    pub fn flatten_tree(&self, tree_hash: &Hash, prefix: &str) -> Result<BTreeMap<String, Hash>> {
        let tree = self.read_tree(tree_hash)?;
        let mut map = BTreeMap::new();
        for entry in &tree.entries {
            let path = if prefix.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", prefix, entry.name)
            };
            if entry.is_dir {
                map.extend(self.flatten_tree(&entry.hash, &path)?);
            } else {
                map.insert(path, entry.hash.clone());
            }
        }
        Ok(map)
    }

    fn load_head_map_cache(&self) -> Option<HeadMapCache> {
        let path = self.bait_dir.join("head-map-cache");
        let bytes = std::fs::read(path).ok()?;
        const MAGIC: &[u8; 4] = b"BHMC";
        if !bytes.starts_with(MAGIC) {
            return None;
        }
        let compressed = &bytes[MAGIC.len()..];
        let raw = zstd::decode_all(std::io::Cursor::new(compressed)).ok()?;
        bincode::deserialize(&raw).ok()
    }

    fn save_head_map_cache(&self, cache: &HeadMapCache) -> Result<()> {
        const MAGIC: &[u8; 4] = b"BHMC";
        let raw = bincode::serialize(cache).context("failed to serialise head map cache")?;
        let compressed = zstd::encode_all(std::io::Cursor::new(raw), 1)
            .context("failed to compress head map cache")?;
        let mut bytes = Vec::with_capacity(MAGIC.len() + compressed.len());
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&compressed);
        std::fs::write(self.bait_dir.join("head-map-cache"), bytes)
            .context("failed to write head map cache")?;
        Ok(())
    }

    /// Walk the working directory and return a flat map of `rel-path → content hash`.
    fn flatten_workdir(&self, ignore: &IgnoreRules) -> Result<BTreeMap<String, Hash>> {
        let mut cache = StatCache::load(&self.bait_dir).unwrap_or_default();
        let mut map = BTreeMap::new();
        let mut cache_dirty = false;

        for entry in walkdir::WalkDir::new(&self.workdir)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| {
                if !e.file_type().is_dir() {
                    return true;
                }
                let path = e.path();
                if path.starts_with(&self.bait_dir) {
                    return false;
                }
                let rel = path
                    .strip_prefix(&self.workdir)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                !ignore.is_ignored(Path::new(&rel))
            })
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.starts_with(&self.bait_dir) {
                continue;
            }
            let rel = path
                .strip_prefix(&self.workdir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");

            if ignore.is_ignored(Path::new(&rel)) {
                continue;
            }

            if entry.file_type().is_file() {
                let metadata = entry.metadata()?;
                let (size, mtime_ns) = metadata_signature(&metadata);

                if let Some(cached) = cache.entries.get(&rel) {
                    if cached.size == size && cached.mtime_ns == mtime_ns {
                        map.insert(rel.clone(), cached.hash.clone());
                        continue;
                    }
                }

                let data = std::fs::read(path)?;
                // Hash must match write_blob: blake3(bincode(Blob { data }))
                let blob = crate::objects::Blob { data: data.to_vec() };
                let blob_bytes = bincode::serialize(&blob).context("failed to serialise blob for hash")?;
                let hash_bytes = *blake3::hash(&blob_bytes).as_bytes();
                let hash = Hash(hash_bytes);
                map.insert(rel.clone(), hash.clone());

                cache.entries.insert(
                    rel,
                    CachedFile {
                        size,
                        mtime_ns,
                        hash,
                    },
                );
                cache_dirty = true;
            }
        }

        if cache_dirty {
            let _ = cache.save(&self.bait_dir);
        }

        Ok(map)
    }

    // ─── Config helpers ───────────────────────────────────────────────────────

    /// Persist the in-memory config back to `.bait/config`.
    pub fn save_config(&self) -> Result<()> {
        let toml_str =
            toml::to_string_pretty(&self.config).context("failed to serialise config")?;
        std::fs::write(self.bait_dir.join("config"), toml_str)
            .context("failed to write config")?;
        Ok(())
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Split a repo-relative path like `"src/foo/bar.rs"` into `("src/foo", "bar.rs")`.
fn split_path(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(idx) => (path[..idx].to_string(), path[idx + 1..].to_string()),
        None => (String::new(), path.to_string()),
    }
}

/// Return `true` when the file has the executable bit set (Unix).
#[allow(unused_variables)]
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        false
    }
}
