use crate::{ignore::IgnoreRules, repo::Repository, stat_cache::metadata_signature};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const AI_INDEX_DIR: &str = "ai-index";
const AI_INDEX_FILE: &str = "symbols.idx";
const AI_INDEX_MAGIC: &[u8; 5] = b"BAI1\0";
const MAX_DOC_SUMMARY_LEN: usize = 160;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Struct,
    Enum,
    Trait,
    Module,
    Impl,
    Type,
    Const,
    Static,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub name: String,
    pub kind: SymbolKind,
    pub file: String,
    pub line: usize,
    pub module: String,
    pub is_public: bool,
    pub doc_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleRecord {
    pub module: String,
    pub file: String,
    pub exports: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedFile {
    pub size: u64,
    pub mtime_ns: u128,
    pub module: String,
    pub symbols: Vec<SymbolRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AiIndex {
    pub files: BTreeMap<String, IndexedFile>,
    pub modules: BTreeMap<String, ModuleRecord>,
}

impl AiIndex {
    pub fn load(bait_dir: &Path) -> Result<Self> {
        let path = bait_dir.join(AI_INDEX_DIR).join(AI_INDEX_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }

        let bytes = std::fs::read(&path).context("failed to read AI index file")?;
        if !bytes.starts_with(AI_INDEX_MAGIC) {
            return Ok(Self::default());
        }

        let raw = zstd::decode_all(std::io::Cursor::new(&bytes[AI_INDEX_MAGIC.len()..]))
            .context("failed to decompress AI index")?;
        bincode::deserialize(&raw).context("failed to deserialize AI index")
    }

    pub fn save(&self, bait_dir: &Path) -> Result<()> {
        let dir = bait_dir.join(AI_INDEX_DIR);
        std::fs::create_dir_all(&dir).context("failed to create AI index directory")?;

        let raw = bincode::serialize(self).context("failed to serialize AI index")?;
        let compressed = zstd::encode_all(std::io::Cursor::new(raw), 3)
            .context("failed to compress AI index")?;

        let mut bytes = Vec::with_capacity(AI_INDEX_MAGIC.len() + compressed.len());
        bytes.extend_from_slice(AI_INDEX_MAGIC);
        bytes.extend_from_slice(&compressed);

        std::fs::write(dir.join(AI_INDEX_FILE), bytes).context("failed to write AI index")?;
        Ok(())
    }

    pub fn refresh(repo: &Repository, ignore: &IgnoreRules) -> Result<Self> {
        let mut index = Self::load(&repo.bait_dir).unwrap_or_default();
        let mut seen = BTreeSet::new();

        for entry in walkdir::WalkDir::new(&repo.workdir)
            .min_depth(1)
            .into_iter()
            .filter_entry(|e| {
                let path = e.path();
                if path.starts_with(&repo.bait_dir) {
                    return false;
                }
                let rel = path
                    .strip_prefix(&repo.workdir)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                !ignore.is_ignored(Path::new(&rel))
            })
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            if !is_indexable_file(path) {
                continue;
            }

            let rel = path
                .strip_prefix(&repo.workdir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            seen.insert(rel.clone());

            let metadata = entry.metadata()?;
            let (size, mtime_ns) = metadata_signature(&metadata);
            let mtime_ns = u128::from(mtime_ns);

            let unchanged = index
                .files
                .get(&rel)
                .map(|f| f.size == size && f.mtime_ns == mtime_ns)
                .unwrap_or(false);

            if unchanged {
                continue;
            }

            let content = std::fs::read_to_string(path).unwrap_or_default();
            let module = module_name_for_path(&rel);
            let symbols = extract_symbols(&content, &rel, &module);

            index.files.insert(
                rel.clone(),
                IndexedFile {
                    size,
                    mtime_ns,
                    module,
                    symbols,
                },
            );
        }

        index.files.retain(|path, _| seen.contains(path));
        index.rebuild_modules();
        index.save(&repo.bait_dir)?;
        Ok(index)
    }

    pub fn find_symbol(&self, query: &str, prefix: bool) -> Vec<SymbolRecord> {
        let needle = query.trim();
        if needle.is_empty() {
            return Vec::new();
        }

        let lowered = needle.to_lowercase();
        let mut out = Vec::new();
        for file in self.files.values() {
            for symbol in &file.symbols {
                let matched = if prefix {
                    symbol.name.to_lowercase().starts_with(&lowered)
                } else {
                    symbol.name.eq_ignore_ascii_case(needle)
                };
                if matched {
                    out.push(symbol.clone());
                }
            }
        }

        out.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(a.file.cmp(&b.file))
                .then(a.line.cmp(&b.line))
        });
        out
    }

    pub fn module_records(&self) -> Vec<ModuleRecord> {
        self.modules.values().cloned().collect()
    }

    pub fn total_size_bytes(bait_dir: &Path) -> u64 {
        let dir = bait_dir.join(AI_INDEX_DIR);
        if !dir.exists() {
            return 0;
        }

        walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum()
    }

    fn rebuild_modules(&mut self) {
        self.modules.clear();
        for (file, indexed) in &self.files {
            let exports = indexed
                .symbols
                .iter()
                .filter(|s| s.is_public)
                .map(|s| s.name.clone())
                .collect();

            self.modules.insert(
                indexed.module.clone(),
                ModuleRecord {
                    module: indexed.module.clone(),
                    file: file.clone(),
                    exports,
                },
            );
        }
    }
}

fn is_indexable_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(
            "rs" | "ts" | "tsx" | "js" | "jsx" | "py" | "java" | "go" | "c" | "cc"
                | "cpp" | "h" | "hpp" | "cs" | "rb" | "php" | "swift" | "kt" | "kts"
                | "scala" | "sh"
        )
    )
}

fn module_name_for_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path.strip_suffix(".rs").or_else(|| path.rsplit_once('.').map(|(base, _)| base)).unwrap_or(&path);
    path.replace('/', "::")
}

fn extract_symbols(content: &str, file: &str, module: &str) -> Vec<SymbolRecord> {
    let mut out = Vec::new();
    let mut pending_docs: Vec<String> = Vec::new();

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            let text = trimmed[3..].trim();
            if !text.is_empty() {
                pending_docs.push(text.to_string());
            }
            continue;
        }

        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }

        if let Some((kind, is_public, name)) = parse_symbol_line(trimmed) {
            let doc_summary = if pending_docs.is_empty() {
                None
            } else {
                let joined = pending_docs.join(" ");
                Some(joined.chars().take(MAX_DOC_SUMMARY_LEN).collect())
            };

            out.push(SymbolRecord {
                name,
                kind,
                file: file.to_string(),
                line: idx + 1,
                module: module.to_string(),
                is_public,
                doc_summary,
            });
        }

        pending_docs.clear();
    }

    out
}

fn parse_symbol_line(line: &str) -> Option<(SymbolKind, bool, String)> {
    let normalized = line
        .trim_start_matches("export default ")
        .trim_start_matches("export ")
        .trim_start_matches("async ")
        .trim_start_matches("unsafe ");

    for (needle, kind) in [
        ("pub fn ", SymbolKind::Function),
        ("fn ", SymbolKind::Function),
        ("function ", SymbolKind::Function),
        ("pub struct ", SymbolKind::Struct),
        ("struct ", SymbolKind::Struct),
        ("class ", SymbolKind::Struct),
        ("pub enum ", SymbolKind::Enum),
        ("enum ", SymbolKind::Enum),
        ("pub trait ", SymbolKind::Trait),
        ("trait ", SymbolKind::Trait),
        ("interface ", SymbolKind::Trait),
        ("pub mod ", SymbolKind::Module),
        ("mod ", SymbolKind::Module),
        ("impl ", SymbolKind::Impl),
        ("pub type ", SymbolKind::Type),
        ("type ", SymbolKind::Type),
        ("pub const ", SymbolKind::Const),
        ("const ", SymbolKind::Const),
        ("pub static ", SymbolKind::Static),
        ("static ", SymbolKind::Static),
    ] {
        if let Some(rest) = normalized.strip_prefix(needle) {
            let is_public = line.trim_start().starts_with("pub ") || line.trim_start().starts_with("export ");
            let name = symbol_name_from_rest(rest, &kind)?;
            return Some((kind, is_public, name));
        }
    }

    for prefix in ["const ", "let ", "var "] {
        if let Some(rest) = normalized.strip_prefix(prefix) {
            let is_public = line.trim_start().starts_with("export ");
            let name = symbol_name_from_rest(rest, &SymbolKind::Const)?;
            return Some((SymbolKind::Const, is_public, name));
        }
    }

    None
}

fn symbol_name_from_rest(rest: &str, kind: &SymbolKind) -> Option<String> {
    match kind {
        SymbolKind::Impl => {
            let target = rest.split_whitespace().next()?;
            Some(target.trim_matches('{').trim_matches('<').to_string())
        }
        _ => {
            let mut name = String::new();
            for ch in rest.chars() {
                if ch.is_alphanumeric() || ch == '_' {
                    name.push(ch);
                } else {
                    break;
                }
            }
            if name.is_empty() {
                None
            } else {
                Some(name)
            }
        }
    }
}