use crate::{ignore::IgnoreRules, repo::Repository, stat_cache::metadata_signature};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const AI_INDEX_DIR: &str = "ai-index";
const AI_INDEX_FILE: &str = "symbols.idx";
const AI_INDEX_MAGIC: &[u8; 5] = b"BAI2\0"; // bumped: new SymbolKind variants
const MAX_DOC_SUMMARY_LEN: usize = 160;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SymbolKind {
    // ── original (keep order for bincode compat) ──────────────────────────────
    Function,
    Struct,
    Enum,
    Trait,
    Module,
    Impl,
    Type,
    Const,
    Static,
    // ── new ───────────────────────────────────────────────────────────────────
    Interface,   // Java interface, TS interface, Go interface, C# interface
    Class,       // Java/PHP/C# class (distinct from Rust struct)
    Method,      // explicit method outside an impl block (Java, C#, PHP…)
    Variable,    // field / local / shell / SQL column
    Namespace,   // C++ namespace, C# namespace, Elixir module alias
    Macro,       // Rust macro_rules!, C #define
    Decorator,   // Python @decorator, Java @Annotation header
    Protocol,    // Swift protocol, Elixir protocol
    Extension,   // Swift extension
    Object,      // Kotlin object, JS/TS object literal export
    Record,      // Java record, C# record
    Package,     // Go package, Java package
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
            // Systems
            "rs" | "c" | "cc" | "cpp" | "cxx" | "c++" | "h" | "hpp" | "hxx"
            // JVM
            | "java" | "kt" | "kts" | "scala" | "groovy" | "gradle"
            // .NET
            | "cs" | "fs" | "vb"
            // Web / JS ecosystem
            | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "vue" | "svelte"
            // Python
            | "py" | "pyi" | "pyw"
            // Ruby
            | "rb" | "rake" | "gemspec"
            // PHP
            | "php"
            // Go
            | "go"
            // Swift
            | "swift"
            // Shell
            | "sh" | "bash" | "zsh" | "fish"
            // Functional
            | "ex" | "exs"          // Elixir
            | "erl" | "hrl"         // Erlang
            | "hs" | "lhs"          // Haskell
            | "ml" | "mli"          // OCaml
            | "clj" | "cljs" | "cljc" // Clojure
            | "elm"
            | "jl"                  // Julia
            | "r" | "R"             // R
            | "lua"
            | "dart"
            // Data / query
            | "sql"
            | "graphql" | "gql"
            // Config-as-code
            | "tf" | "tfvars"       // Terraform HCL
            | "bicep"
            | "proto"               // Protobuf
        )
    )
}

fn module_name_for_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path.strip_suffix(".rs").or_else(|| path.rsplit_once('.').map(|(base, _)| base)).unwrap_or(&path);
    path.replace('/', "::")
}

fn extract_symbols(content: &str, file: &str, module: &str) -> Vec<SymbolRecord> {
    let ext = file.rsplit('.').next().unwrap_or("");
    let mut out = Vec::new();
    let mut pending_docs: Vec<String> = Vec::new();
    let mut in_block_comment = false;

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        // ── Block comment tracking ─────────────────────────────────────────────
        if in_block_comment {
            if trimmed.contains("*/") { in_block_comment = false; }
            continue;
        }
        if trimmed.starts_with("/*") {
            // JavaDoc / C block — treat as doc if `/**`
            if trimmed.starts_with("/**") {
                let text = trimmed.trim_start_matches('/').trim_start_matches('*').trim();
                if !text.is_empty() { pending_docs.push(text.to_string()); }
            }
            if !trimmed.contains("*/") { in_block_comment = true; }
            continue;
        }
        if trimmed.starts_with("* ") || trimmed == "*" {
            // inside /** */ block
            let text = trimmed.trim_start_matches('*').trim();
            if !text.is_empty() && !text.starts_with('/') {
                pending_docs.push(text.to_string());
            }
            continue;
        }

        // ── Rust/JS/TS/C/Java // doc comments ─────────────────────────────────
        if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            let text = trimmed[3..].trim();
            if !text.is_empty() { pending_docs.push(text.to_string()); }
            continue;
        }

        // ── Python / Ruby / Shell / Elixir # doc comments ─────────────────────
        if trimmed.starts_with("##") && matches!(ext, "py"|"pyi"|"rb"|"rake"|"sh"|"bash"|"zsh"|"fish"|"ex"|"exs"|"r"|"R") {
            let text = trimmed[2..].trim();
            if !text.is_empty() { pending_docs.push(text.to_string()); }
            continue;
        }

        // ── Skip blank / comment lines ─────────────────────────────────────────
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with('#') && !matches!(ext, "ex"|"exs"|"rb"|"rake"|"py"|"pyi"|"sh"|"bash"|"zsh"|"r"|"R") {
            pending_docs.clear();
            continue;
        }

        if let Some((kind, is_public, name)) = parse_symbol_line(trimmed, ext) {
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

fn parse_symbol_line(line: &str, ext: &str) -> Option<(SymbolKind, bool, String)> {
    let trimmed = line.trim_start();

    // ── Python ────────────────────────────────────────────────────────────────
    if matches!(ext, "py" | "pyi" | "pyw") {
        if let Some(rest) = trimmed.strip_prefix("async def ").or_else(|| trimmed.strip_prefix("def ")) {
            let name = ident(rest)?;
            return Some((SymbolKind::Function, !name.starts_with('_'), name));
        }
        if let Some(rest) = trimmed.strip_prefix("class ") {
            let name = ident(rest)?;
            return Some((SymbolKind::Class, !name.starts_with('_'), name));
        }
        if trimmed.starts_with('@') { return Some((SymbolKind::Decorator, true, ident(&trimmed[1..])?)); }
        if let Some((lhs, _)) = trimmed.split_once(" = ") {
            let name = ident(lhs.trim())?;
            if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                return Some((SymbolKind::Const, true, name));
            }
        }
        return None;
    }

    // ── Ruby ──────────────────────────────────────────────────────────────────
    if matches!(ext, "rb" | "rake" | "gemspec") {
        if let Some(rest) = trimmed.strip_prefix("def self.").or_else(|| trimmed.strip_prefix("def ")) {
            let name = ident(rest)?;
            return Some((SymbolKind::Function, !name.starts_with('_'), name));
        }
        if let Some(rest) = trimmed.strip_prefix("class ") { return Some((SymbolKind::Class, true, ident(rest)?)); }
        if let Some(rest) = trimmed.strip_prefix("module ") { return Some((SymbolKind::Module, true, ident(rest)?)); }
        if let Some(rest) = trimmed.strip_prefix("attr_accessor :").or_else(|| trimmed.strip_prefix("attr_reader :")).or_else(|| trimmed.strip_prefix("attr_writer :")) {
            return Some((SymbolKind::Variable, true, ident(rest)?));
        }
        if let Some((lhs, _)) = trimmed.split_once(" = ") {
            let name = ident(lhs.trim())?;
            if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                return Some((SymbolKind::Const, true, name));
            }
        }
        return None;
    }

    // ── Go ────────────────────────────────────────────────────────────────────
    if ext == "go" {
        if trimmed.starts_with("package ") { return Some((SymbolKind::Package, true, ident(&trimmed[8..])?)); }
        if let Some(rest) = trimmed.strip_prefix("func ") {
            let name_part = if rest.starts_with('(') {
                rest.find(')').and_then(|i| rest.get(i + 1..))?.trim_start()
            } else { rest };
            let name = ident(name_part)?;
            let is_public = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
            return Some((SymbolKind::Function, is_public, name));
        }
        if let Some(rest) = trimmed.strip_prefix("type ") {
            let name = ident(rest)?;
            let is_public = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
            let kind = if rest.contains(" struct") { SymbolKind::Struct }
                else if rest.contains(" interface") { SymbolKind::Interface }
                else { SymbolKind::Type };
            return Some((kind, is_public, name));
        }
        if trimmed.starts_with("var ") || trimmed.starts_with("const ") {
            let rest = &trimmed[if trimmed.starts_with("var ") { 4 } else { 6 }..];
            let name = ident(rest)?;
            let is_public = name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false);
            let kind = if trimmed.starts_with("const ") { SymbolKind::Const } else { SymbolKind::Variable };
            return Some((kind, is_public, name));
        }
        return None;
    }

    // ── Swift ─────────────────────────────────────────────────────────────────
    if ext == "swift" {
        let norm = trimmed
            .trim_start_matches("public ").trim_start_matches("private ").trim_start_matches("internal ")
            .trim_start_matches("open ").trim_start_matches("fileprivate ").trim_start_matches("final ")
            .trim_start_matches("override ").trim_start_matches("static ").trim_start_matches("class ")
            .trim_start_matches("async ").trim_start_matches("mutating ");
        let is_public = trimmed.starts_with("public ") || trimmed.starts_with("open ");
        if let Some(r) = norm.strip_prefix("func ") { return Some((SymbolKind::Function, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("struct ") { return Some((SymbolKind::Struct, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("class ") { return Some((SymbolKind::Class, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("enum ") { return Some((SymbolKind::Enum, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("protocol ") { return Some((SymbolKind::Protocol, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("extension ") { return Some((SymbolKind::Extension, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("typealias ") { return Some((SymbolKind::Type, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("var ") { return Some((SymbolKind::Variable, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("let ") { return Some((SymbolKind::Const, is_public, ident(r)?)); }
        return None;
    }

    // ── Kotlin ────────────────────────────────────────────────────────────────
    if matches!(ext, "kt" | "kts") {
        let norm = trimmed
            .trim_start_matches("public ").trim_start_matches("private ").trim_start_matches("protected ")
            .trim_start_matches("internal ").trim_start_matches("open ").trim_start_matches("abstract ")
            .trim_start_matches("sealed ").trim_start_matches("data ").trim_start_matches("inline ")
            .trim_start_matches("suspend ").trim_start_matches("override ").trim_start_matches("companion ");
        let is_public = !trimmed.starts_with("private ") && !trimmed.starts_with("protected ");
        if let Some(r) = norm.strip_prefix("fun ") { return Some((SymbolKind::Function, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("class ") { return Some((SymbolKind::Class, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("object ") { return Some((SymbolKind::Object, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("interface ") { return Some((SymbolKind::Interface, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("enum class ") { return Some((SymbolKind::Enum, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("typealias ") { return Some((SymbolKind::Type, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("val ") { return Some((SymbolKind::Const, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("var ") { return Some((SymbolKind::Variable, is_public, ident(r)?)); }
        return None;
    }

    // ── Scala ─────────────────────────────────────────────────────────────────
    if ext == "scala" {
        let norm = trimmed.trim_start_matches("override ").trim_start_matches("abstract ")
            .trim_start_matches("sealed ").trim_start_matches("case ").trim_start_matches("lazy ");
        let is_public = !trimmed.starts_with("private ") && !trimmed.starts_with("protected ");
        if let Some(r) = norm.strip_prefix("def ") { return Some((SymbolKind::Function, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("class ") { return Some((SymbolKind::Class, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("object ") { return Some((SymbolKind::Object, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("trait ") { return Some((SymbolKind::Trait, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("type ") { return Some((SymbolKind::Type, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("val ") { return Some((SymbolKind::Const, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("var ") { return Some((SymbolKind::Variable, is_public, ident(r)?)); }
        return None;
    }

    // ── Java ──────────────────────────────────────────────────────────────────
    if ext == "java" {
        for vis in &["public ", "protected ", "private ", ""] {
            let rest = if vis.is_empty() { trimmed } else { match trimmed.strip_prefix(vis) { Some(r) => r, None => continue } };
            let is_public = *vis == "public " || vis.is_empty();
            let norm = rest.trim_start_matches("static ").trim_start_matches("final ").trim_start_matches("abstract ").trim_start_matches("synchronized ").trim_start_matches("native ").trim_start_matches("default ");
            if let Some(r) = norm.strip_prefix("class ") { return Some((SymbolKind::Class, is_public, ident(r)?)); }
            if let Some(r) = norm.strip_prefix("interface ") { return Some((SymbolKind::Interface, is_public, ident(r)?)); }
            if let Some(r) = norm.strip_prefix("enum ") { return Some((SymbolKind::Enum, is_public, ident(r)?)); }
            if let Some(r) = norm.strip_prefix("record ") { return Some((SymbolKind::Record, is_public, ident(r)?)); }
            if let Some(r) = norm.strip_prefix("@interface ") { return Some((SymbolKind::Decorator, is_public, ident(r)?)); }
            if let Some(paren) = norm.find('(') {
                let before = norm[..paren].trim();
                let parts: Vec<&str> = before.split_whitespace().collect();
                if parts.len() >= 2 {
                    if let Some(name) = ident(parts.last().unwrap_or(&"")) {
                        if name.chars().next().map(|c| c.is_lowercase()).unwrap_or(false) {
                            return Some((SymbolKind::Method, is_public, name));
                        }
                    }
                }
            }
        }
        return None;
    }

    // ── C# ────────────────────────────────────────────────────────────────────
    if ext == "cs" {
        let norm = trimmed
            .trim_start_matches("public ").trim_start_matches("private ").trim_start_matches("protected ")
            .trim_start_matches("internal ").trim_start_matches("static ").trim_start_matches("abstract ")
            .trim_start_matches("virtual ").trim_start_matches("override ").trim_start_matches("sealed ")
            .trim_start_matches("partial ").trim_start_matches("async ").trim_start_matches("readonly ");
        let is_public = trimmed.starts_with("public ");
        if let Some(r) = norm.strip_prefix("class ") { return Some((SymbolKind::Class, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("interface ") { return Some((SymbolKind::Interface, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("enum ") { return Some((SymbolKind::Enum, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("struct ") { return Some((SymbolKind::Struct, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("record ") { return Some((SymbolKind::Record, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("namespace ") { return Some((SymbolKind::Namespace, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("delegate ") { return Some((SymbolKind::Type, is_public, ident(r)?)); }
        if let Some(paren) = norm.find('(') {
            let before = norm[..paren].trim();
            let parts: Vec<&str> = before.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Some(name) = ident(parts.last().unwrap_or(&"")) {
                    if !name.is_empty() { return Some((SymbolKind::Method, is_public, name)); }
                }
            }
        }
        return None;
    }

    // ── C / C++ ───────────────────────────────────────────────────────────────
    if matches!(ext, "c" | "cc" | "cpp" | "cxx" | "c++" | "h" | "hpp" | "hxx") {
        if let Some(rest) = trimmed.strip_prefix("#define ") { return Some((SymbolKind::Macro, true, ident(rest)?)); }
        if let Some(rest) = trimmed.strip_prefix("namespace ") { return Some((SymbolKind::Namespace, true, ident(rest)?)); }
        let norm = trimmed.trim_start_matches("static ").trim_start_matches("inline ").trim_start_matches("extern ").trim_start_matches("virtual ").trim_start_matches("explicit ");
        if let Some(r) = norm.strip_prefix("class ") { return Some((SymbolKind::Class, true, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("struct ") { return Some((SymbolKind::Struct, true, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("enum class ").or_else(|| norm.strip_prefix("enum ")) { return Some((SymbolKind::Enum, true, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("typedef ") { return Some((SymbolKind::Type, true, ident(r)?)); }
        if let Some(paren) = norm.find('(') {
            let before = norm[..paren].trim();
            let parts: Vec<&str> = before.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Some(name) = ident(parts.last().unwrap_or(&"")) {
                    if !name.is_empty() && !name.starts_with('*') {
                        return Some((SymbolKind::Function, true, name));
                    }
                }
            }
        }
        return None;
    }

    // ── PHP ───────────────────────────────────────────────────────────────────
    if ext == "php" {
        let norm = trimmed.trim_start_matches("public ").trim_start_matches("private ").trim_start_matches("protected ").trim_start_matches("static ").trim_start_matches("abstract ").trim_start_matches("final ");
        let is_public = !trimmed.starts_with("private ") && !trimmed.starts_with("protected ");
        if let Some(r) = norm.strip_prefix("function ") { return Some((SymbolKind::Function, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("class ") { return Some((SymbolKind::Class, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("interface ") { return Some((SymbolKind::Interface, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("trait ") { return Some((SymbolKind::Trait, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("enum ") { return Some((SymbolKind::Enum, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("const ") { return Some((SymbolKind::Const, is_public, ident(r)?)); }
        return None;
    }

    // ── Shell ─────────────────────────────────────────────────────────────────
    if matches!(ext, "sh" | "bash" | "zsh" | "fish") {
        if let Some(rest) = trimmed.strip_prefix("function ") { return Some((SymbolKind::Function, true, ident(rest)?)); }
        if let Some(paren) = trimmed.find("() {").or_else(|| trimmed.find("(){")) {
            let name = ident(&trimmed[..paren])?;
            if !name.is_empty() { return Some((SymbolKind::Function, true, name)); }
        }
        if let Some((lhs, _)) = trimmed.split_once('=') {
            let name = ident(lhs.trim())?;
            if name.chars().all(|c| c.is_uppercase() || c == '_') && name.len() > 1 {
                return Some((SymbolKind::Variable, true, name));
            }
        }
        return None;
    }

    // ── Elixir ────────────────────────────────────────────────────────────────
    if matches!(ext, "ex" | "exs") {
        if let Some(r) = trimmed.strip_prefix("defp ") { return Some((SymbolKind::Function, false, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("def ") { return Some((SymbolKind::Function, true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("defmacro ") { return Some((SymbolKind::Macro, true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("defmodule ") { return Some((SymbolKind::Module, true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("defprotocol ") { return Some((SymbolKind::Protocol, true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("defimpl ") { return Some((SymbolKind::Impl, true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("defstruct ") { return Some((SymbolKind::Struct, true, ident(r)?)); }
        return None;
    }

    // ── SQL ───────────────────────────────────────────────────────────────────
    if ext == "sql" {
        let up = trimmed.to_uppercase();
        let find_after = |keyword: &str| -> Option<String> {
            up.find(keyword).and_then(|i| ident(&trimmed[i + keyword.len()..]))
        };
        if up.contains("TABLE ") { return Some((SymbolKind::Struct, true, find_after("TABLE ")?)); }
        if up.contains("VIEW ") { return Some((SymbolKind::Type, true, find_after("VIEW ")?)); }
        if up.contains("FUNCTION ") { return Some((SymbolKind::Function, true, find_after("FUNCTION ")?)); }
        if up.contains("PROCEDURE ") { return Some((SymbolKind::Function, true, find_after("PROCEDURE ")?)); }
        return None;
    }

    // ── GraphQL ───────────────────────────────────────────────────────────────
    if matches!(ext, "graphql" | "gql") {
        for (prefix, kind) in &[
            ("type ", SymbolKind::Struct), ("interface ", SymbolKind::Interface),
            ("enum ", SymbolKind::Enum), ("input ", SymbolKind::Struct),
            ("union ", SymbolKind::Type), ("fragment ", SymbolKind::Function),
            ("query ", SymbolKind::Function), ("mutation ", SymbolKind::Function),
            ("subscription ", SymbolKind::Function), ("scalar ", SymbolKind::Type),
            ("directive ", SymbolKind::Decorator),
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                return Some((kind.clone(), true, ident(rest)?));
            }
        }
        return None;
    }

    // ── Lua ───────────────────────────────────────────────────────────────────
    if ext == "lua" {
        if let Some(rest) = trimmed.strip_prefix("local function ").or_else(|| trimmed.strip_prefix("function ")) {
            return Some((SymbolKind::Function, !trimmed.starts_with("local "), ident(rest)?));
        }
        if let Some((lhs, _)) = trimmed.split_once(" = function") {
            return Some((SymbolKind::Function, !trimmed.starts_with("local "), ident(lhs.trim_start_matches("local ").trim())?));
        }
        return None;
    }

    // ── Dart ──────────────────────────────────────────────────────────────────
    if ext == "dart" {
        let norm = trimmed
            .trim_start_matches("abstract ").trim_start_matches("mixin ").trim_start_matches("base ")
            .trim_start_matches("final ").trim_start_matches("interface ").trim_start_matches("sealed ");
        let is_public = !trimmed.starts_with('_');
        if let Some(r) = norm.strip_prefix("class ") { return Some((SymbolKind::Class, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("enum ") { return Some((SymbolKind::Enum, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("typedef ") { return Some((SymbolKind::Type, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("extension ") { return Some((SymbolKind::Extension, is_public, ident(r)?)); }
        if let Some(paren) = norm.find('(') {
            let parts: Vec<&str> = norm[..paren].trim().split_whitespace().collect();
            if parts.len() >= 2 { return Some((SymbolKind::Function, is_public, ident(parts.last().unwrap_or(&""))?)); }
        }
        return None;
    }

    // ── Protobuf ──────────────────────────────────────────────────────────────
    if ext == "proto" {
        if let Some(r) = trimmed.strip_prefix("message ") { return Some((SymbolKind::Struct, true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("service ") { return Some((SymbolKind::Interface, true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("enum ") { return Some((SymbolKind::Enum, true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("rpc ") { return Some((SymbolKind::Function, true, ident(r)?)); }
        return None;
    }

    // ── Terraform / Bicep ─────────────────────────────────────────────────────
    if matches!(ext, "tf" | "tfvars" | "bicep") {
        if let Some(rest) = trimmed.strip_prefix("resource \"") {
            // resource "type" "name" { -> use the name part (3rd token)
            if let Some(name) = rest.splitn(3, '"').nth(2).and_then(|s| ident(s.trim_start_matches('"').trim())) {
                return Some((SymbolKind::Struct, true, name));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("variable \"") { return Some((SymbolKind::Variable, true, ident(rest)?)); }
        if let Some(rest) = trimmed.strip_prefix("module \"") { return Some((SymbolKind::Module, true, ident(rest)?)); }
        if ext == "bicep" {
            if let Some(r) = trimmed.strip_prefix("param ") { return Some((SymbolKind::Variable, true, ident(r)?)); }
            if let Some(r) = trimmed.strip_prefix("var ") { return Some((SymbolKind::Variable, true, ident(r)?)); }
            if let Some(r) = trimmed.strip_prefix("resource ") { return Some((SymbolKind::Struct, true, ident(r)?)); }
            if let Some(r) = trimmed.strip_prefix("module ") { return Some((SymbolKind::Module, true, ident(r)?)); }
        }
        return None;
    }

    // ── Rust ──────────────────────────────────────────────────────────────────
    if ext == "rs" {
        let norm = trimmed
            .trim_start_matches("pub(crate) ").trim_start_matches("pub(super) ")
            .trim_start_matches("pub ").trim_start_matches("unsafe ").trim_start_matches("async ");
        let is_public = trimmed.starts_with("pub");
        if let Some(r) = norm.strip_prefix("fn ") { return Some((SymbolKind::Function, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("struct ") { return Some((SymbolKind::Struct, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("enum ") { return Some((SymbolKind::Enum, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("trait ") { return Some((SymbolKind::Trait, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("mod ") { return Some((SymbolKind::Module, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("impl ") { return Some((SymbolKind::Impl, true, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("type ") { return Some((SymbolKind::Type, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("const ") { return Some((SymbolKind::Const, is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("static ") { return Some((SymbolKind::Static, is_public, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("macro_rules! ") { return Some((SymbolKind::Macro, true, ident(r)?)); }
        return None;
    }

    // ── JavaScript / TypeScript (default fallthrough) ─────────────────────────
    {
        let is_export = trimmed.starts_with("export ") || trimmed.starts_with("export default ");
        let norm = trimmed
            .trim_start_matches("export default ")
            .trim_start_matches("export ")
            .trim_start_matches("declare ")
            .trim_start_matches("abstract ")
            .trim_start_matches("async ");
        if let Some(r) = norm.strip_prefix("function ") { return Some((SymbolKind::Function, is_export, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("class ") { return Some((SymbolKind::Class, is_export, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("interface ") { return Some((SymbolKind::Interface, is_export, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("enum ") { return Some((SymbolKind::Enum, is_export, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("type ") { return Some((SymbolKind::Type, is_export, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("namespace ") { return Some((SymbolKind::Namespace, is_export, ident(r)?)); }
        if trimmed.starts_with('@') { return Some((SymbolKind::Decorator, true, ident(&trimmed[1..])?)); }
        for prefix in ["const ", "let ", "var "] {
            if let Some(rest) = norm.strip_prefix(prefix) {
                let name = ident(rest)?;
                let after = rest[name.len()..].trim_start();
                if is_export { return Some((SymbolKind::Const, true, name)); }
                if after.starts_with("= (") || after.starts_with("= async (") || after.starts_with("= function") {
                    return Some((SymbolKind::Function, false, name));
                }
            }
        }
    }

    None
}

/// Extract a valid identifier (alphanumeric + `_`) from the start of `s`.
fn ident(s: &str) -> Option<String> {
    let s = s.trim_start();
    let mut name = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() || ch == '_' { name.push(ch); } else { break; }
    }
    if name.is_empty() { None } else { Some(name) }
}