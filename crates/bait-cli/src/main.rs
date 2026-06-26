mod locale;

use anyhow::{bail, Context, Result};
use bait_core::{
    ai_index::AiIndex,
    collab::{issues_dir, pull_requests_dir, IssueRecord, PullRequestRecord},
    ignore::IgnoreRules,
    index::Index,
    objects::{Commit, Hash},
    oplog::Operation,
    repo::Repository,
    stat_cache::{metadata_signature, CachedFile, StatCache},
};
use chrono::{DateTime, TimeZone, Utc};
use clap::{Parser, Subcommand};
use colored::Colorize;
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "bait",
    about = "Bharat's Autonomous Integrated Tracker — a new kind of version control",
    version,
    long_about = None,
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialise a new bait repository in the current directory.
    Init {
        /// Path to initialise (defaults to current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Stage files for the next save.
    Add {
        /// Files or glob patterns to stage.
        #[arg(required = true, num_args = 1..)]
        paths: Vec<String>,
    },

    /// Snapshot staged files (or the entire working copy) as a new commit.
    Save {
        /// Commit message.
        #[arg(short, long)]
        message: Option<String>,
        /// Author name (alias: --rushi). Overrides config user.name.
        #[arg(long, alias = "rushi")]
        author: Option<String>,
        /// Author email.
        #[arg(long)]
        email: Option<String>,
        /// Reviewer name (alias: --narada).
        #[arg(long, alias = "narada")]
        reviewer: Option<String>,
        /// Reviewer email.
        #[arg(long)]
        reviewer_email: Option<String>,
    },

    /// Show commit history.
    Log {
        /// Maximum number of commits to show.
        #[arg(short = 'n', long, default_value = "20")]
        limit: usize,
    },

    /// Show the status of the working copy.
    Status,

    /// Show a diff of the working copy against HEAD.
    Diff {
        /// File to diff (diffs all files when omitted).
        file: Option<String>,
    },

    /// Branch management.
    Branch {
        #[command(subcommand)]
        action: BranchAction,
    },

    /// Switch to a branch.
    Switch {
        /// Branch name.
        name: String,
        /// Create the branch if it doesn't exist.
        #[arg(short = 'c', long)]
        create: bool,
    },

    /// Merge another branch into the current branch.
    Merge {
        /// Branch name to merge from.
        from: String,
    },

    /// Undo the last operation.
    Undo,

    /// Prune local branches.
    Clean {
        /// Branches to keep (space-separated list after the flag).
        #[arg(long = "except", num_args = 0..)]
        except: Vec<String>,
        /// Actually delete (without this flag the command only shows what would be deleted).
        #[arg(long)]
        yes: bool,
    },

    /// Remote repository management.
    Remote {
        #[command(subcommand)]
        action: RemoteAction,
    },

    /// Synchronise with a remote or local clone.
    Sync {
        /// Sync with a local filesystem path.
        #[arg(long, conflicts_with = "remote")]
        local: Option<PathBuf>,
        /// Sync with the named remote (defaults to "origin").
        #[arg(long)]
        remote: Option<String>,
    },

    /// Code review management (narada).
    Review {
        #[command(subcommand)]
        action: ReviewAction,
    },

    /// Issue tracking.
    Issue {
        #[command(subcommand)]
        action: IssueAction,
    },

    /// Pull request tracking.
    PullRequest {
        #[command(subcommand)]
        action: PullRequestAction,
    },

    /// Show or set user configuration.
    Config {
        /// Config key (e.g. user.name).
        key: Option<String>,
        /// Value to set.
        value: Option<String>,
    },

    /// Manage local repository hooks.
    Hooks {
        #[command(subcommand)]
        action: HookAction,
    },

    /// Storage analysis commands.
    Storage {
        #[command(subcommand)]
        action: StorageAction,
    },

    /// Lightweight AI-oriented code index and lookup commands.
    Ai {
        #[command(subcommand)]
        action: AiAction,
    },
}

#[derive(Subcommand)]
enum BranchAction {
    /// List branches.
    List,
    /// Create a new branch.
    New {
        /// Branch name.
        name: String,
    },
    /// Delete a branch.
    Delete {
        /// Branch name.
        name: String,
        /// Force deletion even if the branch has unmerged commits.
        #[arg(short, long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum RemoteAction {
    /// Add a remote.
    Add {
        /// Remote name (e.g. "origin").
        name: String,
        /// Remote URL.
        url: String,
    },
    /// List configured remotes.
    List,
    /// Remove a remote.
    Remove {
        /// Remote name.
        name: String,
    },
}

#[derive(Subcommand)]
enum ReviewAction {
    /// Create a new review request.
    Create {
        /// Reviewer name (alias: --narada).
        #[arg(long, alias = "narada")]
        reviewer: Option<String>,
        /// Reviewer email.
        #[arg(long)]
        reviewer_email: Option<String>,
        /// Description of the review.
        #[arg(short, long)]
        message: Option<String>,
    },
    /// List open reviews.
    List,
}

#[derive(Subcommand)]
enum IssueAction {
    /// Create a new issue.
    Create {
        /// Issue title.
        #[arg(short, long)]
        title: String,
        /// Issue body.
        #[arg(short, long)]
        body: Option<String>,
        /// Labels for the issue.
        #[arg(long, num_args = 0..)]
        labels: Vec<String>,
    },
    /// List issues.
    List,
    /// Close an issue.
    Close {
        /// Issue ID to close.
        id: usize,
    },
}

#[derive(Subcommand)]
enum PullRequestAction {
    /// Create a new pull request from the current branch.
    Create {
        /// Base branch to merge into.
        #[arg(short, long, default_value = "main")]
        base: String,
        /// Pull request title.
        #[arg(short, long)]
        title: Option<String>,
        /// Pull request body.
        #[arg(short, long)]
        body: Option<String>,
        /// Reviewer name (alias: --narada).
        #[arg(long, alias = "narada")]
        reviewer: Option<String>,
        /// Reviewer email.
        #[arg(long)]
        reviewer_email: Option<String>,
    },
    /// List pull requests.
    List,
    /// Close a pull request without merging.
    Close {
        /// Pull request ID to close.
        id: usize,
    },
    /// Merge a pull request (must be on the base branch).
    Merge {
        /// Pull request ID to merge.
        id: usize,
    },
}

#[derive(Subcommand)]
enum HookAction {
    /// List installed hooks.
    List,
    /// Install a sample hook script.
    InstallSample {
        /// Hook name.
        name: String,
    },
}

#[derive(Subcommand)]
enum StorageAction {
    /// Show object store and index footprint.
    Stats,
}

#[derive(Subcommand)]
enum AiAction {
    /// Build or refresh the AI symbol index.
    Index {
        /// Drop the current index and rebuild from scratch.
        #[arg(long)]
        rebuild: bool,
    },
    /// Find symbols by exact or prefix match.
    Find {
        /// Symbol name to search for.
        query: String,
        /// Use prefix matching instead of exact matching.
        #[arg(long)]
        prefix: bool,
    },
    /// List indexed modules and exported symbol counts.
    Modules,
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() {
    locale::init();

    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("{} {}", "error:".red().bold(), e);
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Init { path } => cmd_init(&path),
        Commands::Add { paths } => cmd_add(paths),
        Commands::Save {
            message,
            author,
            email,
            reviewer,
            reviewer_email,
        } => cmd_save(message, author, email, reviewer, reviewer_email),
        Commands::Log { limit } => cmd_log(limit),
        Commands::Status => cmd_status(),
        Commands::Diff { file } => cmd_diff(file),
        Commands::Branch { action } => cmd_branch(action),
        Commands::Switch { name, create } => cmd_switch(&name, create),
        Commands::Merge { from } => cmd_merge(&from),
        Commands::Undo => cmd_undo(),
        Commands::Clean { except, yes } => cmd_clean(except, yes),
        Commands::Remote { action } => cmd_remote(action),
        Commands::Sync { local, remote } => cmd_sync(local, remote),
        Commands::Review { action } => cmd_review(action),
        Commands::Issue { action } => cmd_issue(action),
        Commands::PullRequest { action } => cmd_pull_request(action),
        Commands::Config { key, value } => cmd_config(key, value),
        Commands::Hooks { action } => cmd_hooks(action),
        Commands::Storage { action } => cmd_storage(action),
        Commands::Ai { action } => cmd_ai(action),
    }
}

// ── Command implementations ───────────────────────────────────────────────────

fn cmd_init(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path).context("failed to create directory")?;
    match Repository::init(path) {
        Ok(repo) => {
            println!(
                "{}",
                locale::t("init_success").replace("{path}", &repo.bait_dir.display().to_string())
            );
            Ok(())
        }
        Err(e) if e.to_string().contains("already exists") => {
            bail!(
                "{}",
                locale::t("init_exists").replace("{path}", &path.display().to_string())
            )
        }
        Err(e) => Err(e),
    }
}

fn cmd_add(patterns: Vec<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let ignore = IgnoreRules::load(&repo.workdir, &repo.bait_dir)?;
    let mut index = Index::load(&repo.bait_dir)?;
    let mut stat_cache = StatCache::load(&repo.bait_dir).unwrap_or_default();
    let mut cache_dirty = false;

    let mut staged_count = 0usize;

    let mut stage_file = |file: &Path| -> Result<()> {
        let rel = file
            .strip_prefix(&repo.workdir)
            .unwrap_or(file)
            .to_string_lossy()
            .replace('\\', "/");

        if ignore.is_ignored(Path::new(&rel)) {
            return Ok(());
        }

        if !file.is_file() {
            return Ok(());
        }

        let metadata = std::fs::metadata(file)
            .with_context(|| format!("failed to stat {}", file.display()))?;
        let (size, mtime_ns) = metadata_signature(&metadata);

        let hash = if let Some(cached) = stat_cache.entries.get(&rel) {
            if cached.size == size && cached.mtime_ns == mtime_ns {
                if index
                    .entries
                    .get(&rel)
                    .map(|e| &e.hash == &cached.hash)
                    .unwrap_or(false)
                {
                    cached.hash.clone()
                } else if repo.store.exists(&cached.hash) {
                    cached.hash.clone()
                } else {
                    let data = std::fs::read(file)
                        .with_context(|| format!("failed to read {}", file.display()))?;
                    let hash = repo.write_blob(&data)?;
                    stat_cache.entries.insert(
                        rel.clone(),
                        CachedFile {
                            size,
                            mtime_ns,
                            hash: hash.clone(),
                        },
                    );
                    cache_dirty = true;
                    hash
                }
            } else {
                let data = std::fs::read(file)
                    .with_context(|| format!("failed to read {}", file.display()))?;
                let hash = repo.write_blob(&data)?;
                stat_cache.entries.insert(
                    rel.clone(),
                    CachedFile {
                        size,
                        mtime_ns,
                        hash: hash.clone(),
                    },
                );
                cache_dirty = true;
                hash
            }
        } else {
            let data = std::fs::read(file)
                .with_context(|| format!("failed to read {}", file.display()))?;
            let hash = repo.write_blob(&data)?;
            stat_cache.entries.insert(
                rel.clone(),
                CachedFile {
                    size,
                    mtime_ns,
                    hash: hash.clone(),
                },
            );
            cache_dirty = true;
            hash
        };

        let executable = is_executable_metadata(&metadata);
        if index
            .entries
            .get(&rel)
            .map(|e| e.hash == hash && e.executable == executable)
            .unwrap_or(false)
        {
            return Ok(());
        }

        index.add(rel, hash, executable);
        staged_count += 1;
        Ok(())
    };

    for pattern in &patterns {
        let path = cwd.join(pattern);

        if path.is_dir() {
            for entry in walkdir::WalkDir::new(&path)
                .into_iter()
                .filter_entry(|e| {
                    if !e.file_type().is_dir() {
                        return true;
                    }
                    let p = e.path();
                    if p.starts_with(&repo.bait_dir) {
                        return false;
                    }
                    let rel = p
                        .strip_prefix(&repo.workdir)
                        .unwrap_or(p)
                        .to_string_lossy()
                        .replace('\\', "/");
                    !ignore.is_ignored(Path::new(&rel))
                })
                .filter_map(|e| e.ok())
            {
                if entry.file_type().is_file() {
                    stage_file(entry.path())?;
                }
            }
            continue;
        }

        if path.is_file() {
            stage_file(&path)?;
            continue;
        }

        {
            // Treat as a glob pattern relative to cwd.
            let glob = glob::glob(&cwd.join(pattern).to_string_lossy())
                .or_else(|_| glob::glob(pattern));
            match glob {
                Ok(paths) => {
                    let mut matched = false;
                    for file in paths.filter_map(|p| p.ok()).filter(|p| p.is_file()) {
                        matched = true;
                        stage_file(&file)?;
                    }
                    if !matched {
                        eprintln!(
                            "{}",
                            locale::t("add_no_match").replace("{pattern}", pattern)
                        );
                    }
                }
                Err(_) => {
                    eprintln!(
                        "{}",
                        locale::t("add_no_match").replace("{pattern}", pattern)
                    );
                }
            }
        }
    }

    index.save(&repo.bait_dir)?;
    if cache_dirty {
        let _ = stat_cache.save(&repo.bait_dir);
    }

    if staged_count > 0 {
        println!(
            "{}",
            locale::t("add_staged").replace("{count}", &staged_count.to_string())
        );
    }

    Ok(())
}

fn cmd_save(
    message: Option<String>,
    author: Option<String>,
    email: Option<String>,
    reviewer: Option<String>,
    reviewer_email: Option<String>,
) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let mut index = Index::load(&repo.bait_dir)?;

    let auto_snapshot = index.is_empty();
    if auto_snapshot {
        eprintln!("{}", locale::t("save_auto_snap").dimmed());
    }

    // Ask for a message interactively if not provided.
    let msg = match message {
        Some(m) => m,
        None => {
            eprint!("Message: ");
            let mut buf = String::new();
            std::io::stdin()
                .read_line(&mut buf)
                .context("failed to read message")?;
            buf.trim().to_string()
        }
    };

    let rushi = author
        .or_else(|| Some(repo.config.user.name.clone()))
        .unwrap_or_else(|| "unknown".to_string());
    let mail = email
        .or_else(|| Some(repo.config.user.email.clone()))
        .unwrap_or_default();

    run_hook(
        &repo,
        "pre-save",
        &[msg.as_str(), rushi.as_str(), mail.as_str()],
    )?;

    // Snapshot the tree.
    let tree_hash = repo.snapshot_tree(if auto_snapshot { None } else { Some(&index) })?;

    // Check whether the tree changed.
    let previous_head = repo.head_commit()?;
    if let Some(ref prev_hash) = previous_head {
        let prev_commit = repo.read_commit(prev_hash)?;
        if prev_commit.tree == tree_hash {
            println!("{}", locale::t("save_nothing").yellow());
            return Ok(());
        }
    }

    let commit = Commit {
        tree: tree_hash,
        parents: previous_head.iter().cloned().collect(),
        rushi,
        email: mail,
        narada: reviewer,
        narada_email: reviewer_email,
        timestamp: chrono::Utc::now().timestamp(),
        message: msg.clone(),
        has_conflicts: false,
    };

    let commit_hash = repo.write_commit(&commit)?;

    // Update branch pointer.
    repo.set_head_commit(&commit_hash)?;

    // Log the operation.
    repo.oplog.append(Operation::Save {
        branch: repo.current_branch().unwrap_or_else(|_| "main".to_string()),
        commit_hash: commit_hash.clone(),
        previous_head,
    })?;

    // Clear the index.
    index.clear();
    index.save(&repo.bait_dir)?;
    refresh_ai_index_warn(&repo);

    println!(
        "{}",
        locale::t("save_success")
            .replace("{hash}", &commit_hash.short())
            .green()
    );

    run_hook_warn(
        &repo,
        "post-save",
        &[commit_hash.to_hex().as_str(), msg.as_str()],
    );

    Ok(())
}

fn cmd_log(limit: usize) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;

    let mut current = repo.head_commit()?;
    if current.is_none() {
        println!("{}", locale::t("log_empty").dimmed());
        return Ok(());
    }

    let mut shown = 0usize;
    while let Some(hash) = current {
        if shown >= limit {
            break;
        }
        let commit = repo.read_commit(&hash)?;

        println!("{}", locale::t("log_commit_line").replace("{hash}", &hash.to_hex()).yellow());
        println!(
            "{}",
            locale::t("log_rushi_line")
                .replace("{name}", &commit.rushi)
                .replace("{email}", &commit.email)
        );
        if let Some(ref narada) = commit.narada {
            println!("{}", locale::t("log_narada_line").replace("{name}", narada).dimmed());
        }
        let date = format_timestamp(commit.timestamp);
        println!("{}", locale::t("log_date_line").replace("{date}", &date).dimmed());
        println!();
        for line in commit.message.lines() {
            println!("{}", locale::t("log_msg_line").replace("{msg}", line));
        }
        if commit.has_conflicts {
            println!("{}", locale::t("log_has_conflicts").red());
        }
        println!();

        current = commit.parents.into_iter().next();
        shown += 1;
    }

    Ok(())
}

fn cmd_status() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let index = Index::load(&repo.bait_dir)?;
    let ws = repo.working_status(&index)?;

    if ws.staged.is_empty() && ws.unstaged.is_empty() && ws.untracked.is_empty() {
        println!("{}", locale::t("status_clean").green());
        return Ok(());
    }

    let branch = repo.current_branch().unwrap_or_else(|_| "(detached)".to_string());
    println!("On branch {}", branch.cyan().bold());
    println!();

    if !ws.staged.is_empty() {
        println!("{}", locale::t("status_staged_header").green().bold());
        for e in &ws.staged {
            let msg = match e.kind {
                bait_core::repo::StatusKind::Added => {
                    locale::t("status_added").replace("{path}", &e.path)
                }
                bait_core::repo::StatusKind::Modified => {
                    locale::t("status_modified").replace("{path}", &e.path)
                }
                bait_core::repo::StatusKind::Deleted => {
                    locale::t("status_deleted").replace("{path}", &e.path)
                }
            };
            println!("{}", msg.green());
        }
        println!();
    }

    if !ws.unstaged.is_empty() {
        println!("{}", locale::t("status_unstaged_header").red().bold());
        for e in &ws.unstaged {
            let msg = match e.kind {
                bait_core::repo::StatusKind::Added => {
                    locale::t("status_added").replace("{path}", &e.path)
                }
                bait_core::repo::StatusKind::Modified => {
                    locale::t("status_modified").replace("{path}", &e.path)
                }
                bait_core::repo::StatusKind::Deleted => {
                    locale::t("status_deleted").replace("{path}", &e.path)
                }
            };
            println!("{}", msg.red());
        }
        println!();
    }

    if !ws.untracked.is_empty() {
        println!("{}", locale::t("status_untracked_header").dimmed().bold());
        for p in &ws.untracked {
            println!(
                "{}",
                locale::t("status_untracked")
                    .replace("{path}", p)
                    .dimmed()
            );
        }
    }

    Ok(())
}

fn cmd_diff(file: Option<String>) -> Result<()> {
    use bait_core::merge::diff_text;

    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let ignore = IgnoreRules::load(&repo.workdir, &repo.bait_dir)?;

    let head = match repo.head_commit()? {
        Some(h) => h,
        None => {
            println!("{}", locale::t("diff_no_changes").dimmed());
            return Ok(());
        }
    };

    let commit = repo.read_commit(&head)?;
    let head_flat = repo.flatten_tree(&commit.tree, "")?;

    let mut any_diff = false;

    // Collect files to diff.
    let files_to_diff: Vec<PathBuf> = if let Some(ref f) = file {
        vec![repo.workdir.join(f)]
    } else {
        walkdir::WalkDir::new(&repo.workdir)
            .min_depth(1)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file() && !e.path().starts_with(&repo.bait_dir))
            .map(|e| e.path().to_path_buf())
            .collect()
    };

    for abs_path in files_to_diff {
        let rel = abs_path
            .strip_prefix(&repo.workdir)
            .unwrap_or(&abs_path)
            .to_string_lossy()
            .replace('\\', "/");

        if ignore.is_ignored(Path::new(&rel)) {
            continue;
        }

        let working_bytes = match std::fs::read(&abs_path) {
            Ok(b) => b,
            Err(_) => continue,
        };

        // Skip binary files.
        if is_binary(&working_bytes) {
            if let Some(ref head_hash) = head_flat.get(&rel) {
                let head_bytes = repo.read_blob(head_hash)?;
                if working_bytes != head_bytes {
                    println!("{}", locale::t("diff_binary").replace("{path}", &rel).yellow());
                    any_diff = true;
                }
            }
            continue;
        }

        let working_text = String::from_utf8_lossy(&working_bytes).into_owned();
        let head_text = match head_flat.get(&rel) {
            Some(h) => {
                let bytes = repo.read_blob(h)?;
                String::from_utf8_lossy(&bytes).into_owned()
            }
            None => String::new(),
        };

        if working_text != head_text {
            let diff = diff_text(&format!("a/{}", rel), &format!("b/{}", rel), &head_text, &working_text);
            print!("{}", diff);
            any_diff = true;
        }
    }

    if !any_diff {
        println!("{}", locale::t("diff_no_changes").dimmed());
    }

    Ok(())
}

fn cmd_branch(action: BranchAction) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let current = repo.current_branch().unwrap_or_default();

    match action {
        BranchAction::List => {
            let branches = repo.branches.list()?;
            if branches.is_empty() {
                println!("(no branches yet — run 'bait save' to create your first commit)");
                return Ok(());
            }
            for b in branches {
                if b == current {
                    println!("{}", locale::t("branch_list_current").replace("{name}", &b).green().bold());
                } else {
                    println!("{}", locale::t("branch_list_other").replace("{name}", &b));
                }
            }
        }
        BranchAction::New { name } => {
            if repo.branches.exists(&name) {
                bail!("{}", locale::t("branch_exists").replace("{name}", &name));
            }
            // New branch starts at the current HEAD.
            let head = repo.head_commit()?.context(locale::t("err_no_commits"))?;
            repo.branches.write(&name, &head)?;
            repo.oplog.append(Operation::BranchCreate { name: name.clone() })?;
            println!("{}", locale::t("branch_created").replace("{name}", &name).green());
        }
        BranchAction::Delete { name, .. } => {
            if name == current {
                bail!("Cannot delete the currently checked-out branch '{}'", name);
            }
            let hash = repo.branches.read(&name)?
                .with_context(|| locale::t("branch_not_found").replace("{name}", &name))?;
            repo.oplog.append(Operation::BranchDelete {
                name: name.clone(),
                deleted_hash: hash.clone(),
            })?;
            repo.branches.delete(&name)?;
            println!(
                "{}",
                locale::t("branch_deleted")
                    .replace("{name}", &name)
                    .replace("{hash}", &hash.short())
                    .yellow()
            );
        }
    }

    Ok(())
}

fn cmd_switch(name: &str, create: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let current = repo.current_branch().unwrap_or_default();

    if name == current {
        println!("{}", locale::t("switch_same").replace("{name}", name).dimmed());
        return Ok(());
    }

    run_hook(&repo, "pre-switch", &[current.as_str(), name])?;

    // Create the branch if requested.
    if create && !repo.branches.exists(name) {
        let head = repo.head_commit()?;
        if let Some(h) = head {
            repo.branches.write(name, &h)?;
        } else {
            // No commits yet — write an empty ref; will be populated on next save.
            std::fs::create_dir_all(repo.bait_dir.join("refs").join("heads"))?;
            std::fs::write(repo.bait_dir.join("refs").join("heads").join(name), "")?;
        }
    }

    if !repo.branches.exists(name) {
        bail!("{}", locale::t("switch_not_found").replace("{name}", name));
    }

    let previous_head = repo.head_commit()?;

    // Update HEAD.
    std::fs::write(
        repo.bait_dir.join("HEAD"),
        format!("ref: refs/heads/{}\n", name),
    )?;

    // Checkout the branch's commit.
    if let Some(branch_hash) = repo.branches.read(name)? {
        repo.checkout_commit(&branch_hash)
            .context("failed to update working copy during switch")?;
    }

    repo.oplog.append(Operation::Switch {
        from_branch: current.clone(),
        to_branch: name.to_string(),
        previous_head,
    })?;
    refresh_ai_index_warn(&repo);

    println!("{}", locale::t("switch_success").replace("{name}", name).green());

    run_hook_warn(&repo, "post-switch", &[current.as_str(), name]);

    Ok(())
}

fn cmd_merge(from: &str) -> Result<()> {
    use bait_core::merge::merge_text;
    use std::collections::BTreeMap;

    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let onto = repo.current_branch()?;
    run_hook(&repo, "pre-merge", &[from, onto.as_str()])?;

    let onto_head = repo
        .head_commit()?
        .context(locale::t("err_no_commits"))?;
    let from_head = repo
        .branches
        .read(from)?
        .with_context(|| locale::t("branch_not_found").replace("{name}", from))?;

    if onto_head == from_head {
        println!("{}", locale::t("merge_up_to_date").dimmed());
        return Ok(());
    }

    // Find the common ancestor using a simple linear walk.
    let base_hash = find_common_ancestor(&repo, &onto_head, &from_head)?;

    // Get flat file maps for base, ours, theirs.
    let base_commit = repo.read_commit(&base_hash)?;
    let onto_commit = repo.read_commit(&onto_head)?;
    let from_commit = repo.read_commit(&from_head)?;

    let base_files = repo.flatten_tree(&base_commit.tree, "")?;
    let onto_files = repo.flatten_tree(&onto_commit.tree, "")?;
    let from_files = repo.flatten_tree(&from_commit.tree, "")?;

    // Collect all paths.
    let all_paths: std::collections::BTreeSet<String> = base_files
        .keys()
        .chain(onto_files.keys())
        .chain(from_files.keys())
        .cloned()
        .collect();

    let mut merged_entries: BTreeMap<String, (Vec<u8>, bool)> = BTreeMap::new();
    let mut conflict_files: Vec<String> = Vec::new();

    for path in &all_paths {
        let base_bytes = base_files.get(path).map(|h| repo.read_blob(h)).transpose()?;
        let onto_bytes = onto_files.get(path).map(|h| repo.read_blob(h)).transpose()?;
        let from_bytes = from_files.get(path).map(|h| repo.read_blob(h)).transpose()?;

        let merged_bytes: Vec<u8> = match (base_bytes, onto_bytes, from_bytes) {
            // Both deleted — skip.
            (_, None, None) => continue,
            // Only on "from" side — add it.
            (_, None, Some(f)) => f,
            // Only on "onto" side — keep it.
            (_, Some(o), None) => o,
            // Both sides the same as base, or same as each other — use onto.
            (_, Some(o), Some(f)) if o == f => o,
            // 3-way text merge.
            (Some(base_b), Some(onto_b), Some(from_b)) => {
                let base_s = String::from_utf8_lossy(&base_b);
                let onto_s = String::from_utf8_lossy(&onto_b);
                let from_s = String::from_utf8_lossy(&from_b);
                let result = merge_text(&base_s, &onto_s, &from_s);
                let has_conflict = result.has_conflicts();
                let merged_str = result.text().to_string();
                if has_conflict {
                    conflict_files.push(path.clone());
                }
                merged_str.into_bytes()
            }
            // No base — both added differently — conflict.
            (None, Some(onto_b), Some(from_b)) => {
                conflict_files.push(path.clone());
                let conflict_text = format!(
                    "<<<<<<< ours\n{}\n=======\n{}\n>>>>>>> {}\n",
                    String::from_utf8_lossy(&onto_b),
                    String::from_utf8_lossy(&from_b),
                    from
                );
                conflict_text.into_bytes()
            }
        };

        merged_entries.insert(path.clone(), (merged_bytes, false));
    }

    // Write merged files to working directory and rebuild tree.
    let mut tree_entries = Vec::new();
    for (rel_path, (data, executable)) in &merged_entries {
        let abs = repo.workdir.join(rel_path);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&abs, data)?;
        let blob_hash = repo.write_blob(data)?;
        // Only handle flat structure for merged tree (simplified).
        let name = Path::new(rel_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| rel_path.clone());
        tree_entries.push(bait_core::objects::TreeEntry {
            name,
            hash: blob_hash,
            is_dir: false,
            executable: *executable,
        });
    }

    let merge_tree = bait_core::objects::Tree::new(tree_entries);
    let merge_tree_hash = repo.write_tree(&merge_tree)?;

    let merge_commit = Commit {
        tree: merge_tree_hash,
        parents: vec![onto_head.clone(), from_head.clone()],
        rushi: repo.config.user.name.clone(),
        email: repo.config.user.email.clone(),
        narada: None,
        narada_email: None,
        timestamp: chrono::Utc::now().timestamp(),
        message: format!("Merge branch '{}' into '{}'", from, onto),
        has_conflicts: !conflict_files.is_empty(),
    };

    let merge_hash = repo.write_commit(&merge_commit)?;
    repo.set_head_commit(&merge_hash)?;

    repo.oplog.append(Operation::Merge {
        from_branch: from.to_string(),
        onto_branch: onto.clone(),
        merge_commit: merge_hash.clone(),
        previous_head: onto_head,
    })?;

    if conflict_files.is_empty() {
        println!(
            "{}",
            locale::t("merge_success")
                .replace("{from}", from)
                .replace("{onto}", &onto)
                .replace("{hash}", &merge_hash.short())
                .green()
        );
    } else {
        println!(
            "{}",
            locale::t("merge_conflicts")
                .replace("{files}", &conflict_files.join(", "))
                .yellow()
        );
        println!("{}", locale::t("merge_hint").dimmed());
    }

    run_hook_warn(&repo, "post-merge", &[from, onto.as_str(), merge_hash.to_hex().as_str()]);

    Ok(())
}

fn cmd_undo() -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;

    let entry = match repo.oplog.pop_last()? {
        Some(e) => e,
        None => {
            println!("{}", locale::t("undo_nothing").dimmed());
            return Ok(());
        }
    };

    let desc = match &entry.operation {
        Operation::Save { previous_head, branch, .. } => {
            // Restore branch to previous_head.
            match previous_head {
                Some(prev) => {
                    repo.branches.write(branch, prev)?;
                    std::fs::write(
                        repo.bait_dir.join("HEAD"),
                        format!("ref: refs/heads/{}\n", branch),
                    )?;
                    repo.checkout_commit(prev)?;
                }
                None => {
                    // Undo the first commit on this branch — clear the ref.
                    let ref_path = repo.bait_dir.join("refs").join("heads").join(branch);
                    if ref_path.exists() {
                        std::fs::write(&ref_path, "")?;
                    }
                }
            }
            format!("save on '{}'", branch)
        }
        Operation::Switch { from_branch, previous_head, .. } => {
            // Switch back.
            std::fs::write(
                repo.bait_dir.join("HEAD"),
                format!("ref: refs/heads/{}\n", from_branch),
            )?;
            if let Some(prev) = previous_head {
                repo.checkout_commit(prev)?;
            }
            format!("switch to '{}'", from_branch)
        }
        Operation::Merge { onto_branch, previous_head, .. } => {
            // Restore the onto branch to its pre-merge HEAD.
            repo.branches.write(onto_branch, previous_head)?;
            repo.checkout_commit(previous_head)?;
            format!("merge onto '{}'", onto_branch)
        }
        Operation::BranchCreate { name } => {
            repo.branches.delete(name)?;
            format!("branch create '{}'", name)
        }
        Operation::BranchDelete { name, deleted_hash } => {
            repo.branches.write(name, deleted_hash)?;
            format!("branch delete '{}'", name)
        }
        Operation::Clean { deleted_branches } => {
            for (name, hash) in deleted_branches {
                repo.branches.write(name, hash)?;
            }
            "clean".to_string()
        }
    };

    println!("{}", locale::t("undo_success").replace("{op}", &desc).green());

    Ok(())
}

fn cmd_clean(except: Vec<String>, yes: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let current = repo.current_branch().unwrap_or_default();

    // Always keep the current branch and any explicitly excepted ones.
    let mut keep: std::collections::HashSet<String> = except.into_iter().collect();
    keep.insert(current.clone());

    let all_branches = repo.branches.list()?;
    let to_delete: Vec<(String, Hash)> = all_branches
        .into_iter()
        .filter(|b| !keep.contains(b))
        .filter_map(|b| {
            repo.branches.read(&b).ok()?.map(|h| (b, h))
        })
        .collect();

    if to_delete.is_empty() {
        println!("{}", locale::t("clean_nothing").dimmed());
        return Ok(());
    }

    if !yes {
        println!("{}", locale::t("clean_dry_header").yellow().bold());
        for (name, hash) in &to_delete {
            println!(
                "{}",
                locale::t("clean_dry_item")
                    .replace("{name}", name)
                    .replace("{hash}", &hash.short())
                    .dimmed()
            );
        }
        println!();
        println!(
            "{}",
            locale::t("clean_kept")
                .replace("{names}", &keep.iter().cloned().collect::<Vec<_>>().join(", "))
                .dimmed()
        );
        println!("\nRun with --yes to delete.");
        return Ok(());
    }

    let count = to_delete.len();
    repo.oplog.append(Operation::Clean {
        deleted_branches: to_delete.iter().map(|(n, h)| (n.clone(), h.clone())).collect(),
    })?;
    for (name, _) in &to_delete {
        repo.branches.delete(name)?;
    }

    println!(
        "{}",
        locale::t("clean_done")
            .replace("{count}", &count.to_string())
            .green()
    );

    Ok(())
}

fn cmd_remote(action: RemoteAction) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;

    match action {
        RemoteAction::Add { name, url } => {
            // Store remotes in the TOML config as [remote.<name>] url = "..."
            let config_path = repo.bait_dir.join("config");
            let mut raw = std::fs::read_to_string(&config_path)
                .context("failed to read config")?;

            let section = format!("\n[remote.{}]\nurl = \"{}\"\n", name, url);
            if raw.contains(&format!("[remote.{}]", name)) {
                bail!("{}", locale::t("remote_exists").replace("{name}", &name));
            }
            raw.push_str(&section);
            std::fs::write(&config_path, raw)?;
            println!(
                "{}",
                locale::t("remote_added")
                    .replace("{name}", &name)
                    .replace("{url}", &url)
                    .green()
            );
        }
        RemoteAction::List => {
            let config_path = repo.bait_dir.join("config");
            let raw = std::fs::read_to_string(&config_path)?;
            let mut any = false;
            for line in raw.lines() {
                if line.starts_with("[remote.") {
                    let rname = line.trim_start_matches("[remote.").trim_end_matches(']');
                    print!("{}", rname);
                    any = true;
                } else if any && line.trim_start().starts_with("url") {
                    let url = line.split('=').nth(1).map(|s| s.trim().trim_matches('"')).unwrap_or("");
                    println!("\t{}", url);
                    any = false;
                }
            }
        }
        RemoteAction::Remove { name } => {
            let config_path = repo.bait_dir.join("config");
            let raw = std::fs::read_to_string(&config_path)?;
            // Remove the [remote.<name>] section.
            let section_header = format!("[remote.{}]", name);
            if !raw.contains(&section_header) {
                bail!("Remote '{}' not found", name);
            }
            let new_raw = remove_toml_section(&raw, &section_header);
            std::fs::write(&config_path, new_raw)?;
            println!("Remote '{}' removed.", name);
        }
    }

    Ok(())
}

fn cmd_sync(local: Option<PathBuf>, remote: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;

    if let Some(local_path) = local {
        // Local sync: copy objects and refs between two repos.
        let other = Repository::open(&local_path)
            .context("failed to open local sync target")?;
        sync_local(&repo, &other)?;
        refresh_ai_index_warn(&repo);
        println!(
            "{}",
            locale::t("sync_local_done")
                .replace("{path}", &local_path.display().to_string())
                .green()
        );
        return Ok(());
    }

    // Remote sync via HTTP — read URL from config.
    let remote_name = remote.as_deref().unwrap_or("origin");
    let config_path = repo.bait_dir.join("config");
    let config_raw = std::fs::read_to_string(&config_path)?;
    let url = parse_remote_url(&config_raw, remote_name);

    match url {
        Some(u) => {
            sync_remote_http(&repo, remote_name, &u)?;
            refresh_ai_index_warn(&repo);
            println!(
                "{}",
                locale::t("sync_remote_done")
                    .replace("{url}", &u)
                    .green()
            );
        }
        None => {
            bail!("{}", locale::t("sync_no_remote"));
        }
    }

    Ok(())
}

fn cmd_review(action: ReviewAction) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;

    let reviews_dir = repo.bait_dir.join("reviews");
    std::fs::create_dir_all(&reviews_dir)?;

    match action {
        ReviewAction::Create {
            reviewer,
            reviewer_email,
            message,
        } => {
            let branch = repo.current_branch()?;
            let head = repo.head_commit()?.context(locale::t("err_no_commits"))?;

            // Assign a review ID based on count.
            let id = std::fs::read_dir(&reviews_dir)?.count() + 1;

            let review = serde_json::json!({
                "id": id,
                "branch": branch,
                "commit": head.to_hex(),
                "narada": reviewer,
                "narada_email": reviewer_email,
                "message": message,
                "status": "open",
                "created_at": chrono::Utc::now().to_rfc3339(),
            });

            let review_path = reviews_dir.join(format!("{:04}.json", id));
            std::fs::write(&review_path, serde_json::to_string_pretty(&review)?)?;

            println!(
                "{}",
                locale::t("review_created")
                    .replace("{id}", &id.to_string())
                    .replace("{branch}", &branch)
                    .green()
            );
        }
        ReviewAction::List => {
            let mut reviews = Vec::new();
            for entry in std::fs::read_dir(&reviews_dir)? {
                let entry = entry?;
                if entry.path().extension().map_or(false, |e| e == "json") {
                    let raw = std::fs::read_to_string(entry.path())?;
                    let v: serde_json::Value = serde_json::from_str(&raw)?;
                    reviews.push(v);
                }
            }
            if reviews.is_empty() {
                println!("{}", locale::t("review_list_empty").dimmed());
            } else {
                for r in reviews {
                    println!(
                        "#{} [{}] branch: {}  narada: {}",
                        r["id"],
                        r["status"].as_str().unwrap_or("?"),
                        r["branch"].as_str().unwrap_or("?"),
                        r["narada"].as_str().unwrap_or("(none)")
                    );
                }
            }
        }
    }

    Ok(())
}

fn ensure_records_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    Ok(())
}

fn next_record_id(dir: &Path) -> Result<usize> {
    if !dir.exists() {
        return Ok(1);
    }
    Ok(std::fs::read_dir(dir)?.filter_map(|entry| entry.ok()).filter(|entry| entry.path().extension().map_or(false, |ext| ext == "json")).count() + 1)
}

fn load_json_records<T: DeserializeOwned>(dir: &Path) -> Result<Vec<T>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.path().extension().map_or(false, |ext| ext == "json") {
            let raw = std::fs::read_to_string(entry.path())?;
            out.push(serde_json::from_str(&raw)?);
        }
    }
    Ok(out)
}

fn save_json_record<T: Serialize>(dir: &Path, id: usize, value: &T) -> Result<()> {
    ensure_records_dir(dir)?;
    let path = dir.join(format!("{:04}.json", id));
    std::fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

fn update_json_record<T: Serialize + DeserializeOwned>(
    dir: &Path,
    id: usize,
    f: impl FnOnce(&mut T),
) -> Result<()> {
    let path = dir.join(format!("{:04}.json", id));
    if !path.exists() {
        bail!("record #{} not found", id);
    }
    let raw = std::fs::read_to_string(&path)?;
    let mut record: T = serde_json::from_str(&raw)?;
    f(&mut record);
    std::fs::write(&path, serde_json::to_string_pretty(&record)?)?;
    Ok(())
}

fn cmd_issue(action: IssueAction) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let dir = issues_dir(&repo.bait_dir);
    ensure_records_dir(&dir)?;

    match action {
        IssueAction::Create { title, body, labels } => {
            let branch = repo.current_branch()?;
            let head = repo.head_commit()?.context(locale::t("err_no_commits"))?;
            let id = next_record_id(&dir)?;
            let author = repo.config.user.name.clone();
            let author_email = repo.config.user.email.clone();
            let record = IssueRecord {
                id,
                title: title.clone(),
                body,
                labels,
                status: "open".to_string(),
                author,
                author_email,
                branch: branch.clone(),
                commit: head.to_hex(),
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            save_json_record(&dir, id, &record)?;
            println!("Issue #{} created on branch '{}'", id, branch);
        }
        IssueAction::List => {
            let mut issues = load_json_records::<IssueRecord>(&dir)?;
            if issues.is_empty() {
                println!("No issues yet.");
            } else {
                issues.sort_by(|a, b| a.id.cmp(&b.id));
                for issue in issues {
                    let labels = if issue.labels.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", issue.labels.join(", "))
                    };
                    println!("#{} [{}] {}{}", issue.id, issue.status, issue.title, labels);
                    println!("  branch: {}  commit: {}  author: {} <{}>", issue.branch, issue.commit, issue.author, issue.author_email);
                    if let Some(body) = issue.body {
                        println!("  {}", body.lines().next().unwrap_or("").trim());
                    }
                }
            }
        }
        IssueAction::Close { id } => {
            update_json_record::<IssueRecord>(&dir, id, |rec| {
                if rec.status == "closed" {
                    // already closed — no-op
                } else {
                    rec.status = "closed".to_string();
                }
            })?;
            println!("Issue #{} closed.", id);
        }
    }

    Ok(())
}

fn cmd_pull_request(action: PullRequestAction) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let dir = pull_requests_dir(&repo.bait_dir);
    ensure_records_dir(&dir)?;

    match action {
        PullRequestAction::Create {
            base,
            title,
            body,
            reviewer,
            reviewer_email,
        } => {
            let head_branch = repo.current_branch()?;
            if !repo.branches.exists(&base) {
                bail!("base branch '{}' does not exist", base);
            }
            let head_commit = repo.head_commit()?.context(locale::t("err_no_commits"))?;
            let id = next_record_id(&dir)?;
            let author = repo.config.user.name.clone();
            let author_email = repo.config.user.email.clone();
            let record = PullRequestRecord {
                id,
                title: title.unwrap_or_else(|| format!("Merge {} into {}", head_branch, base)),
                body,
                head_branch: head_branch.clone(),
                base_branch: base.clone(),
                head_commit: head_commit.to_hex(),
                status: "open".to_string(),
                author,
                author_email,
                reviewer,
                reviewer_email,
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            save_json_record(&dir, id, &record)?;
            println!("Pull request #{} created from '{}' into '{}'", id, head_branch, base);
        }
        PullRequestAction::List => {
            let mut pulls = load_json_records::<PullRequestRecord>(&dir)?;
            if pulls.is_empty() {
                println!("No pull requests yet.");
            } else {
                pulls.sort_by(|a, b| a.id.cmp(&b.id));
                for pr in pulls {
                    println!("#{} [{}] {}", pr.id, pr.status, pr.title);
                    println!("  {} -> {}  commit: {}  author: {} <{}>", pr.head_branch, pr.base_branch, pr.head_commit, pr.author, pr.author_email);
                    if let Some(reviewer) = pr.reviewer {
                        println!("  reviewer: {}", reviewer);
                    }
                    if let Some(body) = pr.body {
                        println!("  {}", body.lines().next().unwrap_or("").trim());
                    }
                }
            }
        }
        PullRequestAction::Close { id } => {
            update_json_record::<PullRequestRecord>(&dir, id, |rec| {
                rec.status = "closed".to_string();
            })?;
            println!("Pull request #{} closed.", id);
        }
        PullRequestAction::Merge { id } => {
            // Load the PR record first (immutable read).
            let pr: PullRequestRecord = {
                let path = dir.join(format!("{:04}.json", id));
                if !path.exists() {
                    bail!("pull request #{} not found", id);
                }
                serde_json::from_str(&std::fs::read_to_string(&path)?)?
            };
            if pr.status != "open" {
                bail!("pull request #{} is already {}", id, pr.status);
            }
            let current = repo.current_branch()?;
            if current != pr.base_branch {
                bail!(
                    "you are on branch '{}' but PR #{} targets '{}' — switch to '{}' first",
                    current, id, pr.base_branch, pr.base_branch
                );
            }
            // Perform the merge.
            cmd_merge(&pr.head_branch)?;
            // Mark PR as merged.
            update_json_record::<PullRequestRecord>(&dir, id, |rec| {
                rec.status = "merged".to_string();
            })?;
            println!("Pull request #{} merged and closed.", id);
        }
    }

    Ok(())
}

fn cmd_config(key: Option<String>, value: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let mut repo = open_repo(&cwd)?;

    match (key.as_deref(), value.as_deref()) {
        (Some("user.name"), Some(v)) => {
            repo.config.user.name = v.to_string();
            repo.save_config()?;
            println!("user.name = {}", v);
        }
        (Some("user.email"), Some(v)) => {
            repo.config.user.email = v.to_string();
            repo.save_config()?;
            println!("user.email = {}", v);
        }
        (Some(k), None) => {
            println!("(reading config key '{}' — not yet fully implemented)", k);
        }
        (None, _) => {
            println!("user.name  = {}", repo.config.user.name);
            println!("user.email = {}", repo.config.user.email);
        }
        _ => {
            bail!("Unknown config key: {}", key.unwrap_or_default());
        }
    }

    Ok(())
}

fn cmd_hooks(action: HookAction) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;
    let hooks_dir = repo.bait_dir.join("hooks");

    match action {
        HookAction::List => {
            let mut any = false;
            if hooks_dir.is_dir() {
                for entry in std::fs::read_dir(&hooks_dir)? {
                    let entry = entry?;
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }
                    any = true;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let exec = if is_executable(&path) { "(executable)" } else { "(not executable)" };
                    println!("{} {}", name, exec);
                }
            }
            if !any {
                println!("No hooks installed. Use 'bait hooks install-sample <name>'.");
            }
        }
        HookAction::InstallSample { name } => {
            if !is_supported_hook_name(&name) {
                bail!(
                    "unsupported hook '{}'. supported: {}",
                    name,
                    supported_hook_names().join(", ")
                );
            }

            std::fs::create_dir_all(&hooks_dir)?;
            let hook_path = hooks_dir.join(&name);
            if hook_path.exists() {
                bail!("hook '{}' already exists", name);
            }

            let sample = format!(
                "#!/usr/bin/env sh\n# BAIT hook: {}\n# Arguments are provided by the invoking command.\n# Exit non-zero to block operation (for pre hooks).\necho \"[bait hook] {} $@\"\nexit 0\n",
                name, name
            );
            std::fs::write(&hook_path, sample)?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut perms = std::fs::metadata(&hook_path)?.permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&hook_path, perms)?;
            }

            println!("Installed sample hook at {}", hook_path.display());
        }
    }

    Ok(())
}

fn cmd_storage(action: StorageAction) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;

    match action {
        StorageAction::Stats => {
            let objects_dir = repo.bait_dir.join("objects");
            let mut object_files = 0usize;
            let mut total_bytes = 0u64;
            let mut largest: Vec<(u64, String)> = Vec::new();
            let mut shard_counts: Vec<(String, usize)> = Vec::new();

            if objects_dir.exists() {
                for shard_entry in std::fs::read_dir(&objects_dir)? {
                    let shard_entry = shard_entry?;
                    let shard_path = shard_entry.path();
                    if !shard_path.is_dir() {
                        continue;
                    }

                    let shard_name = shard_entry.file_name().to_string_lossy().into_owned();
                    let mut shard_count = 0usize;

                    for obj_entry in std::fs::read_dir(&shard_path)? {
                        let obj_entry = obj_entry?;
                        let obj_path = obj_entry.path();
                        if !obj_path.is_file() {
                            continue;
                        }

                        let meta = obj_entry.metadata()?;
                        let size = meta.len();
                        total_bytes += size;
                        object_files += 1;
                        shard_count += 1;

                        let obj_name = obj_entry.file_name().to_string_lossy().into_owned();
                        let hex = format!("{}{}", shard_name, obj_name);
                        largest.push((size, hex));
                    }

                    if shard_count > 0 {
                        shard_counts.push((shard_name, shard_count));
                    }
                }
            }

            largest.sort_by(|a, b| b.0.cmp(&a.0));
            shard_counts.sort_by(|a, b| b.1.cmp(&a.1));

            let index_path = repo.bait_dir.join("index");
            let index_bytes = std::fs::metadata(&index_path).map(|m| m.len()).unwrap_or(0);
            let ai_index_bytes = AiIndex::total_size_bytes(&repo.bait_dir);

            println!("Object files: {}", object_files);
            println!("Object store size: {} bytes ({:.2} MiB)", total_bytes, total_bytes as f64 / 1024.0 / 1024.0);
            println!("Index size: {} bytes ({:.2} KiB)", index_bytes, index_bytes as f64 / 1024.0);
            println!("AI index size: {} bytes ({:.2} KiB)", ai_index_bytes, ai_index_bytes as f64 / 1024.0);

            if object_files > 0 {
                println!("Average object size: {} bytes", total_bytes / object_files as u64);
            }

            if !largest.is_empty() {
                println!("\nTop largest objects:");
                for (size, hex) in largest.into_iter().take(10) {
                    println!("  {} bytes  {}", size, hex);
                }
            }

            if !shard_counts.is_empty() {
                println!("\nTop shard counts:");
                for (shard, count) in shard_counts.into_iter().take(10) {
                    println!("  {}  {} object(s)", shard, count);
                }
            }
        }
    }

    Ok(())
}

fn cmd_ai(action: AiAction) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let repo = open_repo(&cwd)?;

    match action {
        AiAction::Index { rebuild } => {
            if rebuild {
                let _ = std::fs::remove_dir_all(repo.bait_dir.join("ai-index"));
            }
            let ignore = IgnoreRules::load(&repo.workdir, &repo.bait_dir)?;
            let index = AiIndex::refresh(&repo, &ignore)?;
            let symbol_count: usize = index.files.values().map(|f| f.symbols.len()).sum();
            println!(
                "Built AI index: {} file(s), {} symbol(s)",
                index.files.len(),
                symbol_count
            );
        }
        AiAction::Find { query, prefix } => {
            let index = load_or_refresh_ai_index(&repo)?;
            let results = index.find_symbol(&query, prefix);
            if results.is_empty() {
                println!("No indexed symbols matched '{}'", query);
                return Ok(());
            }

            for symbol in results {
                let visibility = if symbol.is_public { "pub" } else { "priv" };
                println!(
                    "{}\t{:?}\t{}:{}\t{}\t{}",
                    symbol.name,
                    symbol.kind,
                    symbol.file,
                    symbol.line,
                    symbol.module,
                    visibility
                );
                if let Some(doc) = symbol.doc_summary {
                    println!("  {}", doc);
                }
            }
        }
        AiAction::Modules => {
            let index = load_or_refresh_ai_index(&repo)?;
            let mut modules = index.module_records();
            modules.sort_by(|a, b| a.module.cmp(&b.module));
            for module in modules {
                println!("{}\t{} export(s)\t{}", module.module, module.exports.len(), module.file);
            }
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn open_repo(cwd: &Path) -> Result<Repository> {
    Repository::open(cwd).context(locale::t("err_not_a_repo"))
}

fn format_timestamp(ts: i64) -> String {
    DateTime::<Utc>::from_timestamp(ts, 0)
        .unwrap_or_else(|| Utc.timestamp_opt(0, 0).single().unwrap())
        .format("%Y-%m-%d %H:%M:%S UTC")
        .to_string()
}

fn supported_hook_names() -> Vec<&'static str> {
    vec![
        "pre-save",
        "post-save",
        "pre-switch",
        "post-switch",
        "pre-merge",
        "post-merge",
    ]
}

fn is_supported_hook_name(name: &str) -> bool {
    supported_hook_names().contains(&name)
}

fn run_hook(repo: &Repository, name: &str, args: &[&str]) -> Result<()> {
    let hook_path = repo.bait_dir.join("hooks").join(name);
    if !hook_path.exists() {
        return Ok(());
    }

    let status = Command::new(&hook_path)
        .args(args)
        .current_dir(&repo.workdir)
        .env("BAIT_DIR", &repo.bait_dir)
        .env("BAIT_WORKDIR", &repo.workdir)
        .status()
        .with_context(|| format!("failed to execute hook '{}'", name))?;

    if !status.success() {
        bail!("hook '{}' failed with status {}", name, status);
    }

    Ok(())
}

fn run_hook_warn(repo: &Repository, name: &str, args: &[&str]) {
    if let Err(e) = run_hook(repo, name, args) {
        eprintln!("{} {}", "warning:".yellow().bold(), e);
    }
}

fn load_or_refresh_ai_index(repo: &Repository) -> Result<AiIndex> {
    let index = AiIndex::load(&repo.bait_dir).unwrap_or_default();
    if !index.files.is_empty() {
        return Ok(index);
    }

    let ignore = IgnoreRules::load(&repo.workdir, &repo.bait_dir)?;
    AiIndex::refresh(repo, &ignore)
}

fn refresh_ai_index_warn(repo: &Repository) {
    let Some(ignore) = IgnoreRules::load(&repo.workdir, &repo.bait_dir).ok() else {
        return;
    };
    if let Err(err) = AiIndex::refresh(repo, &ignore) {
        eprintln!("{} failed to refresh AI index: {}", "warning:".yellow().bold(), err);
    }
}

/// Walk parent-commit chains to find the common ancestor of `a` and `b`.
fn find_common_ancestor(repo: &Repository, a: &Hash, b: &Hash) -> Result<Hash> {
    use std::collections::HashSet;

    // Collect all ancestors of `a`.
    let mut a_ancestors: HashSet<String> = HashSet::new();
    let mut stack = vec![a.clone()];
    while let Some(h) = stack.pop() {
        let key = h.to_hex();
        if a_ancestors.contains(&key) {
            continue;
        }
        a_ancestors.insert(key);
        let c = repo.read_commit(&h)?;
        stack.extend(c.parents);
    }

    // Walk `b`'s ancestors until we find one in `a_ancestors`.
    let mut stack = vec![b.clone()];
    let mut visited: HashSet<String> = HashSet::new();
    while let Some(h) = stack.pop() {
        let key = h.to_hex();
        if a_ancestors.contains(&key) {
            return Ok(h);
        }
        if visited.contains(&key) {
            continue;
        }
        visited.insert(key);
        let c = repo.read_commit(&h)?;
        stack.extend(c.parents);
    }

    bail!(
        "{}",
        locale::t("merge_no_ancestor")
            .replace("{a}", &a.short())
            .replace("{b}", &b.short())
    )
}

/// Sync objects and refs from `src` into `dst`.
fn sync_local(src: &Repository, dst: &Repository) -> Result<()> {
    // Copy all objects from src to dst.
    let src_objects = src.bait_dir.join("objects");
    for shard in std::fs::read_dir(&src_objects)? {
        let shard = shard?;
        for obj in std::fs::read_dir(shard.path())? {
            let obj = obj?;
            let shard_name = shard.file_name().to_string_lossy().into_owned();
            let obj_name = obj.file_name().to_string_lossy().into_owned();
            let dst_shard = dst.bait_dir.join("objects").join(&shard_name);
            std::fs::create_dir_all(&dst_shard)?;
            let dst_obj = dst_shard.join(&obj_name);
            if !dst_obj.exists() {
                std::fs::copy(obj.path(), dst_obj)?;
            }
        }
    }

    // Copy refs.
    let src_heads = src.bait_dir.join("refs").join("heads");
    let dst_heads = dst.bait_dir.join("refs").join("heads");
    for entry in std::fs::read_dir(&src_heads)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let content = std::fs::read_to_string(entry.path())?;
        let dst_ref = dst_heads.join(&name);
        // Only update if src is newer (simple: always overwrite).
        std::fs::write(dst_ref, content)?;
    }

    Ok(())
}

/// Bidirectional HTTP sync with a bait-server remote.
///
/// - Push: uploads local objects missing on remote and updates remote refs.
/// - Pull: downloads remote objects missing locally and updates remote-tracking refs.
fn sync_remote_http(repo: &Repository, remote_name: &str, remote_url: &str) -> Result<()> {
    let base = remote_url.trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("failed to construct HTTP client")?;

    let local_objects = list_object_hashes(repo)?;
    let remote_objects = fetch_remote_objects(&client, base)?;

    // Push local objects the remote does not have.
    for h in local_objects.difference(&remote_objects) {
        upload_object(repo, &client, base, h)?;
    }

    // Push local branch refs.
    for branch in repo.branches.list()? {
        if let Some(head) = repo.branches.read(&branch)? {
            let url = format!("{}/refs/heads/{}", base, branch);
            let body = serde_json::json!({ "hash": head.to_hex() });
            let resp = client.post(url).json(&body).send();
            match resp {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => {
                    bail!("failed to push ref '{}': HTTP {}", branch, r.status());
                }
                Err(e) => {
                    bail!("failed to push ref '{}': {}", branch, e);
                }
            }
        }
    }

    // Pull any new objects from remote.
    let remote_objects_after_push = fetch_remote_objects(&client, base)?;
    let local_objects_after_push = list_object_hashes(repo)?;
    for h in remote_objects_after_push.difference(&local_objects_after_push) {
        download_object(repo, &client, base, h)?;
    }

    // Pull remote refs into tracking namespace: refs/remotes/<remote>/<branch>
    let remote_refs = fetch_remote_refs(&client, base)?;
    let remotes_dir = repo.bait_dir.join("refs").join("remotes").join(remote_name);
    std::fs::create_dir_all(&remotes_dir)?;
    for (branch, hash) in remote_refs {
        std::fs::write(remotes_dir.join(branch), format!("{}\n", hash))?;
    }

    // Refresh local HEAD map cache after object/ref movement.
    let _ = std::fs::remove_file(repo.bait_dir.join("head-map-cache"));

    Ok(())
}

fn list_object_hashes(repo: &Repository) -> Result<std::collections::HashSet<String>> {
    use std::collections::HashSet;

    let mut out = HashSet::new();
    let objects_dir = repo.bait_dir.join("objects");
    if !objects_dir.exists() {
        return Ok(out);
    }
    for shard in std::fs::read_dir(objects_dir)? {
        let shard = shard?;
        if !shard.path().is_dir() {
            continue;
        }
        let shard_name = shard.file_name().to_string_lossy().into_owned();
        if shard_name.len() != 2 {
            continue;
        }
        for entry in std::fs::read_dir(shard.path())? {
            let entry = entry?;
            if !entry.path().is_file() {
                continue;
            }
            let tail = entry.file_name().to_string_lossy().into_owned();
            if tail.len() == 62 {
                out.insert(format!("{}{}", shard_name, tail));
            }
        }
    }
    Ok(out)
}

fn fetch_remote_objects(
    client: &reqwest::blocking::Client,
    base: &str,
) -> Result<std::collections::HashSet<String>> {
    use std::collections::HashSet;

    let url = format!("{}/objects/list", base);
    let resp = client.get(url).send().context("failed to fetch remote objects")?;
    if !resp.status().is_success() {
        bail!("failed to fetch remote objects: HTTP {}", resp.status());
    }
    let text = resp.text()?;
    let mut out = HashSet::new();
    for line in text.lines() {
        let h = line.trim();
        if h.len() == 64 {
            out.insert(h.to_string());
        }
    }
    Ok(out)
}

fn fetch_remote_refs(
    client: &reqwest::blocking::Client,
    base: &str,
) -> Result<Vec<(String, String)>> {
    let url = format!("{}/refs/heads", base);
    let resp = client.get(url).send().context("failed to fetch remote refs")?;
    if !resp.status().is_success() {
        bail!("failed to fetch remote refs: HTTP {}", resp.status());
    }
    let text = resp.text()?;
    let mut refs = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('\t');
        if let (Some(hash), Some(branch)) = (parts.next(), parts.next()) {
            refs.push((branch.to_string(), hash.to_string()));
        }
    }
    Ok(refs)
}

fn upload_object(
    repo: &Repository,
    client: &reqwest::blocking::Client,
    base: &str,
    hash_hex: &str,
) -> Result<()> {
    let obj_path = repo
        .bait_dir
        .join("objects")
        .join(&hash_hex[..2])
        .join(&hash_hex[2..]);
    let bytes = std::fs::read(&obj_path)
        .with_context(|| format!("failed to read local object {}", hash_hex))?;

    let url = format!("{}/objects", base);
    let resp = client
        .post(url)
        .body(bytes)
        .send()
        .with_context(|| format!("failed to upload object {}", hash_hex))?;

    if !resp.status().is_success() {
        bail!("failed to upload object {}: HTTP {}", hash_hex, resp.status());
    }
    Ok(())
}

fn download_object(
    repo: &Repository,
    client: &reqwest::blocking::Client,
    base: &str,
    hash_hex: &str,
) -> Result<()> {
    let url = format!("{}/objects/{}", base, hash_hex);
    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("failed to download object {}", hash_hex))?;

    if !resp.status().is_success() {
        bail!("failed to download object {}: HTTP {}", hash_hex, resp.status());
    }

    let bytes = resp.bytes()?;
    let shard_dir = repo.bait_dir.join("objects").join(&hash_hex[..2]);
    std::fs::create_dir_all(&shard_dir)?;
    let obj_path = shard_dir.join(&hash_hex[2..]);
    if !obj_path.exists() {
        std::fs::write(obj_path, &bytes)?;
    }
    Ok(())
}

fn is_binary(data: &[u8]) -> bool {
    // Heuristic: if the first 8KB contains a null byte, treat as binary.
    let sample = &data[..data.len().min(8192)];
    sample.contains(&0u8)
}

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
        let _ = path;
        false
    }
}

#[allow(unused_variables)]
fn is_executable_metadata(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn parse_remote_url<'a>(config: &'a str, name: &str) -> Option<String> {
    let section = format!("[remote.{}]", name);
    let mut in_section = false;
    for line in config.lines() {
        let t = line.trim();
        if t == section {
            in_section = true;
            continue;
        }
        if in_section {
            if t.starts_with('[') {
                break;
            }
            if t.starts_with("url") {
                return t
                    .split('=')
                    .nth(1)
                    .map(|s| s.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn remove_toml_section(content: &str, section_header: &str) -> String {
    let mut out = Vec::new();
    let mut skip = false;
    for line in content.lines() {
        let t = line.trim();
        if t == section_header {
            skip = true;
            continue;
        }
        if skip && t.starts_with('[') {
            skip = false;
        }
        if !skip {
            out.push(line);
        }
    }
    out.join("\n") + "\n"
}

// ── glob shim ─────────────────────────────────────────────────────────────────
// We use the `globset` crate internally but need glob-style path expansion here.
// Implement a minimal wrapper that uses globset.
mod glob {
    use globset::Glob;
    use std::path::PathBuf;

    pub struct Paths {
        items: Vec<PathBuf>,
        idx: usize,
    }

    impl Iterator for Paths {
        type Item = Result<PathBuf, ()>;
        fn next(&mut self) -> Option<Self::Item> {
            if self.idx < self.items.len() {
                let item = self.items[self.idx].clone();
                self.idx += 1;
                Some(Ok(item))
            } else {
                None
            }
        }
    }

    pub fn glob(pattern: &str) -> Result<Paths, ()> {
        let gs = Glob::new(pattern)
            .map_err(|_| ())?
            .compile_matcher();

        let base = std::path::Path::new(".");
        let mut items = Vec::new();
        for entry in walkdir::WalkDir::new(base).into_iter().filter_map(|e| e.ok()) {
            let path = entry.path();
            let rel = path.strip_prefix(base).unwrap_or(path);
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if gs.is_match(&rel_str) && path.is_file() {
                items.push(path.to_path_buf());
            }
        }
        Ok(Paths { items, idx: 0 })
    }
}
