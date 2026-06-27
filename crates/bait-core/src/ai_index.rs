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
            // Systems / native
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
            | "ex" | "exs"            // Elixir
            | "erl" | "hrl"           // Erlang
            | "hs" | "lhs"            // Haskell
            | "ml" | "mli"            // OCaml
            | "clj" | "cljs" | "cljc" // Clojure
            | "elm"
            | "jl"                    // Julia
            | "r" | "R"               // R
            | "lua"
            | "dart"
            // Data / query
            | "sql"
            | "graphql" | "gql"
            // Config-as-code
            | "tf" | "tfvars"         // Terraform HCL
            | "bicep"
            | "proto"                 // Protobuf
        )
    )
}

fn module_name_for_path(path: &str) -> String {
    let path = path.replace('\\', "/");
    let path = path
        .strip_suffix(".rs")
        .or_else(|| path.rsplit_once('.').map(|(base, _)| base))
        .unwrap_or(&path);
    path.replace('/', "::")
}

// ─── Tree-sitter queries (one per language) ───────────────────────────────────
// Capture names encode kind: fn / struct / enum / trait / mod / impl / type /
// const / static / class / interface / method / var / ns / macro / decorator /
// proto / ext / obj / record / pkg
// Names starting with '_' are predicate-only helpers and are skipped.

const Q_RUST: &str = r#"
(function_item name: (identifier) @fn)
(struct_item name: (type_identifier) @struct)
(enum_item name: (type_identifier) @enum)
(trait_item name: (type_identifier) @trait)
(mod_item name: (identifier) @mod)
(impl_item type: (type_identifier) @impl)
(type_item name: (type_identifier) @type)
(const_item name: (identifier) @const)
(static_item name: (identifier) @static)
(macro_definition name: (identifier) @macro)
"#;

const Q_PYTHON: &str = r#"
(function_definition name: (identifier) @fn)
(async_function_definition name: (identifier) @fn)
(class_definition name: (identifier) @class)
(decorator (identifier) @decorator)
(decorator (call function: (identifier) @decorator))
"#;

const Q_JS: &str = r#"
(function_declaration name: (identifier) @fn)
(generator_function_declaration name: (identifier) @fn)
(class_declaration name: (identifier) @class)
(method_definition key: (property_identifier) @method)
"#;

const Q_TS: &str = r#"
(function_declaration name: (identifier) @fn)
(generator_function_declaration name: (identifier) @fn)
(class_declaration name: (identifier) @class)
(abstract_class_declaration name: (type_identifier) @class)
(method_definition key: (property_identifier) @method)
(interface_declaration name: (type_identifier) @interface)
(type_alias_declaration name: (type_identifier) @type)
(enum_declaration name: (identifier) @enum)
"#;

const Q_JAVA: &str = r#"
(class_declaration name: (identifier) @class)
(interface_declaration name: (identifier) @interface)
(enum_declaration name: (identifier) @enum)
(record_declaration name: (identifier) @record)
(method_declaration name: (identifier) @method)
(constructor_declaration name: (identifier) @fn)
(annotation_type_declaration name: (identifier) @decorator)
"#;

const Q_GO: &str = r#"
(function_declaration name: (identifier) @fn)
(method_declaration name: (field_identifier) @method)
(type_spec name: (type_identifier) @type)
(const_spec name: (identifier) @const)
(var_spec name: (identifier) @var)
(package_clause (package_identifier) @pkg)
"#;

const Q_C: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @fn))
(struct_specifier name: (type_identifier) @struct)
(union_specifier name: (type_identifier) @struct)
(enum_specifier name: (type_identifier) @enum)
(type_definition declarator: (type_identifier) @type)
(preproc_def name: (identifier) @macro)
(preproc_function_def name: (identifier) @macro)
"#;

const Q_CPP: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @fn))
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @fn)))
(class_specifier name: (type_identifier) @class)
(struct_specifier name: (type_identifier) @struct)
(union_specifier name: (type_identifier) @struct)
(enum_specifier name: (type_identifier) @enum)
(namespace_definition name: (namespace_identifier) @ns)
(type_definition declarator: (type_identifier) @type)
(preproc_def name: (identifier) @macro)
(preproc_function_def name: (identifier) @macro)
"#;

const Q_CSHARP: &str = r#"
(class_declaration name: (identifier) @class)
(interface_declaration name: (identifier) @interface)
(struct_declaration name: (identifier) @struct)
(enum_declaration name: (identifier) @enum)
(record_declaration name: (identifier) @record)
(namespace_declaration name: (identifier) @ns)
(method_declaration name: (identifier) @method)
(constructor_declaration name: (identifier) @fn)
(property_declaration name: (identifier) @var)
(delegate_declaration name: (identifier) @type)
"#;

const Q_RUBY: &str = r#"
(method name: (identifier) @fn)
(singleton_method name: (identifier) @fn)
(class name: (constant) @class)
(module name: (constant) @mod)
(assignment left: (constant) @const)
"#;

const Q_SCALA: &str = r#"
(function_definition name: (identifier) @fn)
(class_definition name: (identifier) @class)
(object_definition name: (identifier) @obj)
(trait_definition name: (identifier) @trait)
(type_definition name: (type_identifier) @type)
(val_definition pattern: (identifier) @const)
(var_definition pattern: (identifier) @var)
"#;

const Q_PHP: &str = r#"
(function_definition name: (name) @fn)
(class_declaration name: (name) @class)
(interface_declaration name: (name) @interface)
(trait_declaration name: (name) @trait)
(enum_declaration name: (name) @enum)
(method_declaration name: (name) @method)
"#;

const Q_BASH: &str = r#"
(function_definition name: (word) @fn)
"#;

const Q_ELIXIR: &str = r#"
(call
  target: (identifier) @_def
  (arguments (call target: (identifier) @fn))
  (#match? @_def "^def(p|macro)?$"))
(call
  target: (identifier) @_defm
  (arguments (alias) @mod)
  (#match? @_defm "^defmodule$"))
(call
  target: (identifier) @_defp
  (arguments (alias) @proto)
  (#match? @_defp "^defprotocol$"))
"#;

const Q_LUA: &str = r#"
(function_declaration name: (identifier) @fn)
(local_function_declaration name: (identifier) @fn)
(assignment_statement
  (variable_list (identifier) @fn)
  (expression_list (function_definition)))
"#;

const Q_SWIFT: &str = r#"
(function_declaration name: (simple_identifier) @fn)
(class_declaration name: (type_identifier) @class)
(struct_declaration name: (type_identifier) @struct)
(enum_declaration name: (type_identifier) @enum)
(protocol_declaration name: (type_identifier) @proto)
(typealias_declaration name: (type_identifier) @type)
(extension_declaration (user_type (type_identifier) @ext))
"#;

const Q_HASKELL: &str = r#"
(function name: (variable) @fn)
(data_type name: (type) @type)
(newtype name: (type) @type)
(type_synonym name: (type) @type)
(class_declaration name: (type) @trait)
"#;

const Q_R: &str = r#"
(left_assignment lhs: (identifier) @fn rhs: (function_definition))
(equals_assignment lhs: (identifier) @fn rhs: (function_definition))
"#;

// ─── extract_symbols: try tree-sitter, fall back to line scanner ──────────────

fn extract_symbols(content: &str, file: &str, module: &str) -> Vec<SymbolRecord> {
    let ext = file.rsplit('.').next().unwrap_or("");
    if let Some(syms) = ts_extract(content, file, module, ext) {
        return syms;
    }
    line_scan_symbols(content, file, module, ext)
}

// ─── Tree-sitter extraction ───────────────────────────────────────────────────

fn ts_extract(content: &str, file: &str, module: &str, ext: &str) -> Option<Vec<SymbolRecord>> {
    let (lang, query_src) = ts_lang_query(ext)?;
    let src = content.as_bytes();

    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&lang).ok()?;
    let tree = parser.parse(src, None)?;

    // Compile query; a bad query just means we fall through to line scanner
    let query = tree_sitter::Query::new(&lang, query_src).ok()?;
    let cap_names: Vec<String> = query.capture_names().iter().map(|s| s.to_string()).collect();

    let mut cursor = tree_sitter::QueryCursor::new();
    let mut out: Vec<SymbolRecord> = Vec::new();
    let mut seen: std::collections::HashSet<usize> = std::collections::HashSet::new();

    use tree_sitter::StreamingIterator as _;
    let mut matches = cursor.matches(&query, tree.root_node(), src);
    while let Some(m) = matches.next() {
        for cap in m.captures.iter() {
            let cap_name = &cap_names[cap.index as usize];
            if cap_name.starts_with('_') {
                continue; // predicate-only capture
            }
            let node = cap.node;
            if !seen.insert(node.id()) {
                continue; // deduplicate same node matched by multiple patterns
            }
            let name = match node.utf8_text(src) {
                Ok(n) if !n.is_empty() => n.to_string(),
                _ => continue,
            };
            let line = node.start_position().row + 1;
            let kind = ts_cap_kind(cap_name);
            let is_public = ts_is_public(node, src, ext, &name, cap_name);
            out.push(SymbolRecord {
                name,
                kind,
                file: file.to_string(),
                line,
                module: module.to_string(),
                is_public,
                doc_summary: None,
            });
        }
    }

    Some(out)
}

fn ts_lang_query(ext: &str) -> Option<(tree_sitter::Language, &'static str)> {
    match ext {
        "rs" => Some((tree_sitter_rust::LANGUAGE.into(), Q_RUST)),
        "py" | "pyi" | "pyw" => Some((tree_sitter_python::LANGUAGE.into(), Q_PYTHON)),
        "js" | "jsx" | "mjs" | "cjs" => Some((tree_sitter_javascript::LANGUAGE.into(), Q_JS)),
        "ts" | "vue" | "svelte" => {
            Some((tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(), Q_TS))
        }
        "tsx" => Some((tree_sitter_typescript::LANGUAGE_TSX.into(), Q_TS)),
        "java" => Some((tree_sitter_java::LANGUAGE.into(), Q_JAVA)),
        "go" => Some((tree_sitter_go::LANGUAGE.into(), Q_GO)),
        "c" | "h" => Some((tree_sitter_c::LANGUAGE.into(), Q_C)),
        "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hxx" => {
            Some((tree_sitter_cpp::LANGUAGE.into(), Q_CPP))
        }
        "cs" => Some((tree_sitter_c_sharp::LANGUAGE.into(), Q_CSHARP)),
        "rb" | "rake" | "gemspec" => Some((tree_sitter_ruby::LANGUAGE.into(), Q_RUBY)),
        "scala" => Some((tree_sitter_scala::LANGUAGE.into(), Q_SCALA)),
        "php" => Some((tree_sitter_php::LANGUAGE_PHP.into(), Q_PHP)),
        "sh" | "bash" | "zsh" => Some((tree_sitter_bash::LANGUAGE.into(), Q_BASH)),
        "ex" | "exs" => Some((tree_sitter_elixir::LANGUAGE.into(), Q_ELIXIR)),
        "lua" => Some((tree_sitter_lua::LANGUAGE.into(), Q_LUA)),
        "swift" => Some((tree_sitter_swift::LANGUAGE.into(), Q_SWIFT)),
        "hs" | "lhs" => Some((tree_sitter_haskell::LANGUAGE.into(), Q_HASKELL)),
        "r" | "R" => Some((tree_sitter_r::LANGUAGE.into(), Q_R)),
        _ => None,
    }
}

fn ts_cap_kind(cap: &str) -> SymbolKind {
    match cap {
        "fn" => SymbolKind::Function,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "mod" => SymbolKind::Module,
        "impl" => SymbolKind::Impl,
        "type" => SymbolKind::Type,
        "const" => SymbolKind::Const,
        "static" => SymbolKind::Static,
        "class" => SymbolKind::Class,
        "interface" => SymbolKind::Interface,
        "method" => SymbolKind::Method,
        "var" => SymbolKind::Variable,
        "ns" => SymbolKind::Namespace,
        "macro" => SymbolKind::Macro,
        "decorator" => SymbolKind::Decorator,
        "proto" => SymbolKind::Protocol,
        "ext" => SymbolKind::Extension,
        "obj" => SymbolKind::Object,
        "record" => SymbolKind::Record,
        "pkg" => SymbolKind::Package,
        _ => SymbolKind::Function,
    }
}

fn ts_is_public(
    node: tree_sitter::Node,
    src: &[u8],
    ext: &str,
    name: &str,
    cap: &str,
) -> bool {
    // Helper: text of a node
    let text = |n: tree_sitter::Node| n.utf8_text(src).unwrap_or("").to_string();

    match ext {
        // ── Rust: item is pub if its first child is visibility_modifier ───────
        "rs" => {
            let item = node.parent().unwrap_or(node);
            item.child(0)
                .map(|c| c.kind() == "visibility_modifier")
                .unwrap_or(false)
        }

        // ── Python: name starting with _ is private ───────────────────────────
        "py" | "pyi" | "pyw" => !name.starts_with('_'),

        // ── Go: exported if name starts with uppercase ────────────────────────
        "go" => name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false),

        // ── JS/TS: exported if inside export_statement ────────────────────────
        "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "vue" | "svelte" => {
            let mut n = node.parent();
            for _ in 0..5 {
                match n {
                    Some(p) if p.kind() == "export_statement" => return true,
                    Some(p) => n = p.parent(),
                    None => break,
                }
            }
            false
        }

        // ── Java: public or package-private (not private/protected) ──────────
        "java" => {
            let item = node.parent().unwrap_or(node);
            for i in 0..item.child_count() {
                if let Some(child) = item.child(i as u32) {
                    if child.kind() == "modifiers" {
                        let mods = text(child);
                        return mods.contains("public")
                            || (!mods.contains("private") && !mods.contains("protected"));
                    }
                }
            }
            true // package-private is accessible
        }

        // ── C#: private by default unless public keyword present ─────────────
        "cs" => {
            let item = node.parent().unwrap_or(node);
            for i in 0..item.child_count() {
                if let Some(child) = item.child(i as u32) {
                    let k = child.kind();
                    if k == "modifier" {
                        let m = text(child);
                        if m == "public" || m == "internal" {
                            return true;
                        }
                        if m == "private" || m == "protected" {
                            return false;
                        }
                    }
                }
            }
            false
        }

        // ── C/C++: everything at file scope is public ─────────────────────────
        "c" | "h" | "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hxx" => true,

        // ── Ruby: methods/constants are public by default ─────────────────────
        "rb" | "rake" | "gemspec" => cap != "fn" || !name.starts_with('_'),

        // ── PHP: public unless private/protected modifier present ─────────────
        "php" => {
            let item = node.parent().unwrap_or(node);
            for i in 0..item.child_count() {
                if let Some(child) = item.child(i as u32) {
                    let k = child.kind();
                    if k == "visibility_modifier" || k == "var_modifier" {
                        let m = text(child);
                        if m.contains("private") || m.contains("protected") {
                            return false;
                        }
                        if m.contains("public") {
                            return true;
                        }
                    }
                }
            }
            true
        }

        // ── Swift: public/open = true; private/fileprivate = false; internal = false ──
        "swift" => {
            let item = node.parent().unwrap_or(node);
            for i in 0..item.child_count() {
                if let Some(child) = item.child(i as u32) {
                    let m = text(child);
                    if m == "public" || m == "open" {
                        return true;
                    }
                    if m == "private" || m == "fileprivate" {
                        return false;
                    }
                }
            }
            false // internal by default
        }

        // ── Scala/Kotlin: public by default unless prefix says otherwise ──────
        "scala" | "kt" | "kts" => {
            let item = node.parent().unwrap_or(node);
            let item_text = text(item);
            let prefix = &item_text[..item_text.find(name).unwrap_or(0)];
            !prefix.contains("private ") && !prefix.contains("protected ")
        }

        // ── Elixir: defp is private ───────────────────────────────────────────
        "ex" | "exs" => cap != "fn" || {
            // The function's grandparent call should have target "def" not "defp"
            let call = node.parent().and_then(|p| p.parent()).unwrap_or(node);
            call.child_by_field_name("target")
                .and_then(|t| t.utf8_text(src).ok())
                .map(|t| t == "def")
                .unwrap_or(true)
        },

        // ── Everything else: default to public ────────────────────────────────
        _ => true,
    }
}

// ─── Line-scanner fallback (SQL, GraphQL, Terraform, Protobuf, Kotlin, etc.) ──

fn line_scan_symbols(content: &str, file: &str, module: &str, ext: &str) -> Vec<SymbolRecord> {
    let mut out = Vec::new();
    let mut pending_docs: Vec<String> = Vec::new();
    let mut in_block_comment = false;

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        if trimmed.starts_with("/*") {
            if trimmed.starts_with("/**") {
                let text = trimmed.trim_start_matches('/').trim_start_matches('*').trim();
                if !text.is_empty() {
                    pending_docs.push(text.to_string());
                }
            }
            if !trimmed.contains("*/") {
                in_block_comment = true;
            }
            continue;
        }
        if trimmed.starts_with("* ") || trimmed == "*" {
            let text = trimmed.trim_start_matches('*').trim();
            if !text.is_empty() && !text.starts_with('/') {
                pending_docs.push(text.to_string());
            }
            continue;
        }
        if trimmed.starts_with("///") || trimmed.starts_with("//!") {
            let text = trimmed[3..].trim();
            if !text.is_empty() {
                pending_docs.push(text.to_string());
            }
            continue;
        }
        if trimmed.starts_with("##")
            && matches!(
                ext,
                "py" | "pyi" | "rb" | "rake" | "sh" | "bash" | "zsh" | "fish" | "ex" | "exs"
                    | "r" | "R"
            )
        {
            let text = trimmed[2..].trim();
            if !text.is_empty() {
                pending_docs.push(text.to_string());
            }
            continue;
        }
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || (trimmed.starts_with('#')
                && !matches!(ext, "ex" | "exs" | "rb" | "rake" | "py" | "pyi" | "sh" | "bash" | "zsh" | "r" | "R"))
        {
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

// ─── parse_symbol_line: handles Kotlin + SQL + GraphQL + Terraform + Protobuf ─

fn parse_symbol_line(line: &str, ext: &str) -> Option<(SymbolKind, bool, String)> {
    let trimmed = line.trim_start();

    // ── Kotlin (tree-sitter-kotlin <0.23, incompatible with our runtime) ──────
    if matches!(ext, "kt" | "kts") {
        let norm = trimmed
            .trim_start_matches("public ").trim_start_matches("private ")
            .trim_start_matches("protected ").trim_start_matches("internal ")
            .trim_start_matches("open ").trim_start_matches("abstract ")
            .trim_start_matches("sealed ").trim_start_matches("data ")
            .trim_start_matches("inline ").trim_start_matches("suspend ")
            .trim_start_matches("override ").trim_start_matches("companion ");
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

    // ── SQL ───────────────────────────────────────────────────────────────────
    if ext == "sql" {
        let up = trimmed.to_uppercase();
        let find_after = |keyword: &str| -> Option<String> {
            up.find(keyword).and_then(|i| ident(&trimmed[i + keyword.len()..]))
        };
        if up.contains("TABLE ")     { return Some((SymbolKind::Struct,    true, find_after("TABLE ")?));     }
        if up.contains("VIEW ")      { return Some((SymbolKind::Type,      true, find_after("VIEW ")?));      }
        if up.contains("FUNCTION ")  { return Some((SymbolKind::Function,  true, find_after("FUNCTION ")?));  }
        if up.contains("PROCEDURE ") { return Some((SymbolKind::Function,  true, find_after("PROCEDURE ")?)); }
        return None;
    }

    // ── GraphQL ───────────────────────────────────────────────────────────────
    if matches!(ext, "graphql" | "gql") {
        for (prefix, kind) in &[
            ("type ",         SymbolKind::Struct),
            ("interface ",    SymbolKind::Interface),
            ("enum ",         SymbolKind::Enum),
            ("input ",        SymbolKind::Struct),
            ("union ",        SymbolKind::Type),
            ("fragment ",     SymbolKind::Function),
            ("query ",        SymbolKind::Function),
            ("mutation ",     SymbolKind::Function),
            ("subscription ", SymbolKind::Function),
            ("scalar ",       SymbolKind::Type),
            ("directive ",    SymbolKind::Decorator),
        ] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                return Some((kind.clone(), true, ident(rest)?));
            }
        }
        return None;
    }

    // ── Terraform / Bicep ─────────────────────────────────────────────────────
    if matches!(ext, "tf" | "tfvars" | "bicep") {
        if let Some(rest) = trimmed.strip_prefix("resource \"") {
            if let Some(name) = rest.splitn(3, '"').nth(2).and_then(|s| ident(s.trim_start_matches('"').trim())) {
                return Some((SymbolKind::Struct, true, name));
            }
        }
        if let Some(rest) = trimmed.strip_prefix("variable \"") { return Some((SymbolKind::Variable, true, ident(rest)?)); }
        if let Some(rest) = trimmed.strip_prefix("module \"")   { return Some((SymbolKind::Module,   true, ident(rest)?)); }
        if ext == "bicep" {
            if let Some(r) = trimmed.strip_prefix("param ")    { return Some((SymbolKind::Variable, true, ident(r)?)); }
            if let Some(r) = trimmed.strip_prefix("var ")      { return Some((SymbolKind::Variable, true, ident(r)?)); }
            if let Some(r) = trimmed.strip_prefix("resource ") { return Some((SymbolKind::Struct,   true, ident(r)?)); }
            if let Some(r) = trimmed.strip_prefix("module ")   { return Some((SymbolKind::Module,   true, ident(r)?)); }
        }
        return None;
    }

    // ── Protobuf ──────────────────────────────────────────────────────────────
    if ext == "proto" {
        if let Some(r) = trimmed.strip_prefix("message ") { return Some((SymbolKind::Struct,    true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("service ") { return Some((SymbolKind::Interface, true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("enum ")    { return Some((SymbolKind::Enum,      true, ident(r)?)); }
        if let Some(r) = trimmed.strip_prefix("rpc ")     { return Some((SymbolKind::Function,  true, ident(r)?)); }
        return None;
    }

    // ── Dart (no tree-sitter crate available at compatible version) ────────────
    if ext == "dart" {
        let norm = trimmed
            .trim_start_matches("abstract ").trim_start_matches("mixin ")
            .trim_start_matches("base ").trim_start_matches("final ")
            .trim_start_matches("interface ").trim_start_matches("sealed ");
        let is_public = !trimmed.starts_with('_');
        if let Some(r) = norm.strip_prefix("class ")     { return Some((SymbolKind::Class,     is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("enum ")      { return Some((SymbolKind::Enum,      is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("typedef ")   { return Some((SymbolKind::Type,      is_public, ident(r)?)); }
        if let Some(r) = norm.strip_prefix("extension ") { return Some((SymbolKind::Extension, is_public, ident(r)?)); }
        if let Some(paren) = norm.find('(') {
            let parts: Vec<&str> = norm[..paren].trim().split_whitespace().collect();
            if parts.len() >= 2 { return Some((SymbolKind::Function, is_public, ident(parts.last().unwrap_or(&""))?)); }
        }
        return None;
    }

    // ── Shell (fish only — bash/sh/zsh handled by tree-sitter) ────────────────
    if ext == "fish" {
        if let Some(rest) = trimmed.strip_prefix("function ") { return Some((SymbolKind::Function, true, ident(rest)?)); }
        return None;
    }

    None
}

fn ident(s: &str) -> Option<String> {
    let s = s.trim_start();
    let mut name = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            name.push(ch);
        } else {
            break;
        }
    }
    if name.is_empty() { None } else { Some(name) }
}
