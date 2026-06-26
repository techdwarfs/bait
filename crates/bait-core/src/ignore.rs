use anyhow::{Context, Result};
use globset::{Glob, GlobMatcher};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Compiled set of ignore rules assembled from:
///  1. Global ignore   → `~/.config/bait/ignore`
///  2. Repo-root rules → `<workdir>/.baitignore`
///  3. Per-directory   → `<dir>/.baitignore` (checked lazily at match time)
///
/// Patterns follow the same syntax as `.gitignore`:
///  - Blank lines and lines starting with `#` are ignored.
///  - A leading `!` negates the pattern.
///  - Patterns without `/` match the file name only.
///  - Patterns with `/` are matched against the repo-relative path.
pub struct IgnoreRules {
    workdir: PathBuf,
    /// Precompiled global/root rules.
    rules: Vec<CompiledRule>,
    /// Per-directory cached `.baitignore` rules by parent path.
    dir_cache: RefCell<BTreeMap<String, Vec<CompiledRule>>>,
}

#[derive(Clone)]
struct CompiledRule {
    matcher: GlobMatcher,
    is_negation: bool,
    match_file_name_only: bool,
}

impl IgnoreRules {
    /// Load global + repo-root ignore files.
    pub fn load(workdir: &Path, _bait_dir: &Path) -> Result<Self> {
        let mut raw_rules: Vec<(String, bool)> = Vec::new();

        // 1. Global ignore file.
        if let Some(config_dir) = dirs_for_ignore() {
            let global_ignore = config_dir.join("bait").join("ignore");
            if global_ignore.exists() {
                let content = std::fs::read_to_string(&global_ignore)
                    .context("failed to read global ignore file")?;
                parse_ignore_file(&content, &mut raw_rules);
            }
        }

        // 2. Repo-root .baitignore.
        let root_ignore = workdir.join(".baitignore");
        if root_ignore.exists() {
            let content = std::fs::read_to_string(&root_ignore)
                .context("failed to read .baitignore")?;
            parse_ignore_file(&content, &mut raw_rules);
        }

        let rules = compile_rules(&raw_rules);

        Ok(IgnoreRules {
            workdir: workdir.to_path_buf(),
            rules,
            dir_cache: RefCell::new(BTreeMap::new()),
        })
    }

    /// Return `true` when `rel_path` (relative to the repo root) should be ignored.
    ///
    /// `.bait/` is always ignored regardless of rules.
    pub fn is_ignored(&self, rel_path: &Path) -> bool {
        // Always ignore VCS internals regardless of user rules.
        if rel_path.starts_with(".bait") || rel_path.starts_with(".git") {
            return true;
        }

        let dir_rules = self.load_dir_rules(rel_path);
        let all_rules = self.rules.iter().chain(dir_rules.iter());

        let path_str = rel_path.to_string_lossy().replace('\\', "/");
        let file_name = rel_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let mut ignored = false;

        for rule in all_rules {
            let subject = if rule.match_file_name_only {
                &file_name
            } else {
                &path_str
            };

            if rule.matcher.is_match(subject) {
                ignored = !rule.is_negation;
            }
        }

        ignored
    }

    /// Load rules from a `.baitignore` in the closest parent directory of `rel_path`.
    fn load_dir_rules(&self, rel_path: &Path) -> Vec<CompiledRule> {
        if let Some(parent) = rel_path.parent() {
            if !parent.as_os_str().is_empty() {
                let key = parent.to_string_lossy().replace('\\', "/");

                if let Some(rules) = self.dir_cache.borrow().get(&key) {
                    return rules.clone();
                }

                let dir_ignore = self.workdir.join(parent).join(".baitignore");
                if dir_ignore.exists() {
                    if let Ok(content) = std::fs::read_to_string(&dir_ignore) {
                        let mut raw_rules = Vec::new();
                        parse_ignore_file(&content, &mut raw_rules);
                        let compiled = compile_rules(&raw_rules);
                        self.dir_cache
                            .borrow_mut()
                            .insert(key, compiled.clone());
                        return compiled;
                    }
                }

                self.dir_cache.borrow_mut().insert(key, Vec::new());
            }
        }

        Vec::new()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_ignore_file(content: &str, rules: &mut Vec<(String, bool)>) {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some(pat) = trimmed.strip_prefix('!') {
            rules.push((pat.to_string(), true));
        } else {
            rules.push((trimmed.to_string(), false));
        }
    }
}

fn compile_rules(raw_rules: &[(String, bool)]) -> Vec<CompiledRule> {
    let mut compiled = Vec::new();
    for (pattern, is_negation) in raw_rules {
        if let Ok(glob) = Glob::new(pattern) {
            compiled.push(CompiledRule {
                matcher: glob.compile_matcher(),
                is_negation: *is_negation,
                match_file_name_only: !pattern.contains('/'),
            });
        }
    }
    compiled
}

/// Return the platform config directory (e.g. `~/.config` on Unix).
fn dirs_for_ignore() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(".config"))
}
