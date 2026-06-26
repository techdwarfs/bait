/// Result of a 3-way text merge.
#[derive(Debug)]
pub enum MergeResult {
    /// The merge was clean — the result string is the merged content.
    Clean(String),
    /// The merge had conflicts — the result string contains conflict markers
    /// (`<<<<<<<`, `=======`, `>>>>>>>`).  The file is saved as-is so the
    /// commit can be recorded without blocking the save.
    Conflict(String),
}

impl MergeResult {
    /// Return the merged text regardless of whether conflicts were present.
    pub fn text(&self) -> &str {
        match self {
            MergeResult::Clean(s) | MergeResult::Conflict(s) => s,
        }
    }

    pub fn has_conflicts(&self) -> bool {
        matches!(self, MergeResult::Conflict(_))
    }
}

/// Perform a 3-way merge of `base`, `ours`, and `theirs` (all UTF-8 text).
///
/// Returns `MergeResult::Clean` when there are no conflicts, or
/// `MergeResult::Conflict` when conflict markers were inserted.
///
/// Binary files (those that fail UTF-8 decoding) are handled by the caller —
/// this function only deals with text.
pub fn merge_text(base: &str, ours: &str, theirs: &str) -> MergeResult {
    match diffy::merge(base, ours, theirs) {
        Ok(merged) => MergeResult::Clean(merged),
        Err(with_markers) => MergeResult::Conflict(with_markers),
    }
}

/// Produce a unified-diff string showing changes from `old` to `new`.
/// The output is coloured with ANSI codes when `colour` is true.
pub fn diff_text(old_path: &str, new_path: &str, old: &str, new: &str) -> String {
    use similar::{ChangeTag, TextDiff};

    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();

    // Header lines.
    out.push_str(&format!("--- {}\n", old_path));
    out.push_str(&format!("+++ {}\n", new_path));

    for group in diff.grouped_ops(3) {
        // Compute the hunk header (@@ ... @@).
        let first = group.first().unwrap();
        let _last = group.last().unwrap();
        let old_start = first.old_range().start + 1;
        let new_start = first.new_range().start + 1;
        let old_count: usize = group.iter().map(|op| op.old_range().len()).sum();
        let new_count: usize = group.iter().map(|op| op.new_range().len()).sum();
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_start, old_count, new_start, new_count
        ));

        for op in &group {
            for change in diff.iter_changes(op) {
                let prefix = match change.tag() {
                    ChangeTag::Delete => '-',
                    ChangeTag::Insert => '+',
                    ChangeTag::Equal => ' ',
                };
                out.push(prefix);
                out.push_str(change.value());
                if !change.value().ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }

    out
}

/// Result of merging two entire working trees (for `bait merge`).
#[derive(Debug, Default)]
pub struct TreeMergeResult {
    /// Files that were merged cleanly.
    pub clean: Vec<String>,
    /// Files that have unresolved conflicts.
    pub conflicts: Vec<String>,
    /// Files added by the other branch.
    pub added: Vec<String>,
    /// Files deleted by the other branch.
    pub deleted: Vec<String>,
}

impl TreeMergeResult {
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}
