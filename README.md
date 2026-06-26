# BAIT — Bharat's Autonomous Integrated Tracker


A new kind of version control system designed for AI world with simplicity, clarity, and collaboration across language and cultural boundaries.

**BAIT** (Bharat's Autonomous Integrated Tracker) is a content-addressed distributed version control system written in Rust. It combines the best ideas from Git, Pijul, and Darcs with a focus on:

- **Content-addressed storage** using BLAKE3 hashes
- **Simple branching and merging** without rebasing
- **Integrated code review** (narada — divine reviewer)
- **Multilingual messages** (English, Hindi, Tamil, Bengali, Telugu)
- **Undo-first operations** — every action is logged and reversible
- **Minimal ceremony** — easy to learn, hard to misuse

## Installation

### From Source

Requires Rust 1.95 or later.

```bash
git clone https://github.com/techdwarfs/bait.git
cd bait
cargo build --release
sudo install target/release/bait /usr/local/bin/
```

### macOS (Homebrew) — Coming Soon

```bash
brew install bait
```

### Windows — Coming Soon

Pre-built binaries available via Scoop or Windows Package Manager.

## Quick Start

### Initialize a repository

```bash
mkdir my-project && cd my-project
bait init
```

### Configure your identity

```bash
bait config user.name "Aryabhata"
bait config user.email "arya@iit.ac.in"
```

### Create and commit files

```bash
echo "Hello BAIT" > README.md
bait add README.md
bait save --message "Initial commit"
```

### View history

```bash
bait log
```

```
commit 1bc78fb5d3e49...
rushi:   Aryabhata <arya@iit.ac.in>
date:    2026-06-24 23:23:36 UTC

         Initial commit
```

### Work with branches

```bash
bait branch new dev
bait switch dev
echo "feature" > feature.txt
bait add feature.txt
bait save --message "Add feature"
bait switch main
bait merge dev
```

## Key Concepts

### Rushi and Narada

**Rushi** (ऋषि — sage/seer) is the author who creates commits.

**Narada** (नारद — divine messenger) is an optional reviewer assigned to a commit for code review and feedback.

Every commit records both identities for integrated code review workflows.

### Content Addressing

All objects (blobs, trees, commits) are identified by their BLAKE3 hash. This provides:

- Deduplication (identical content stored once)
- Immutability (hash cannot change without changing content)
- Distribution-friendly (objects can be synced peer-to-peer)

### Staging and Snapshotting

**Easy path (no staging):**
```bash
bait save --message "Save all changes"  # Entire working directory snapshotted
```

**Selective staging:**
```bash
bait add src/          # Stage only src/
bait add README.md     # Stage README.md
bait save --message "Update docs and code"
```

### Undo Everything

Every repository-mutating operation is logged. Undo the last operation:

```bash
bait undo
```

Supported operations:
- `save` — undo a commit
- `switch` — undo a branch switch
- `merge` — undo a merge
- `branch create/delete` — undo branch operations
- `clean` — undo branch pruning

### Local Hooks

BAIT now supports local repository hooks (similar to base Git hooks) for automation and policy checks.

Supported hooks:
- `pre-save`, `post-save`
- `pre-switch`, `post-switch`
- `pre-merge`, `post-merge`

Install a sample hook:

```bash
bait hooks install-sample pre-save
```

List installed hooks:

```bash
bait hooks list
```

Pre hooks can block an operation by exiting with a non-zero code.

### Ignore Files

Create `.baitignore` in the repository root:

```
# Patterns are gitignore-style
*.log
*.tmp
node_modules/
.env

# Negation (!) is supported
!important.log
```

Patterns without `/` match any file name anywhere. Patterns with `/` match against the repository-relative path.

A global ignore file can be placed at `~/.config/bait/ignore`.

## Commands Reference

### Initialization

| Command | Purpose |
|---------|---------|
| `bait init [PATH]` | Create a new repository |
| `bait config [KEY] [VALUE]` | Get or set configuration |

### Committing

| Command | Purpose |
|---------|---------|
| `bait add PATHS...` | Stage files for commit |
| `bait save` | Create a new commit |
| `bait status` | Show working copy status |
| `bait diff [FILE]` | Show changes vs HEAD |

### History

| Command | Purpose |
|---------|---------|
| `bait log [-n N]` | Show commit history |
| `bait undo` | Undo the last operation |

### Branching

| Command | Purpose |
|---------|---------|
| `bait branch list` | List all branches |
| `bait branch new NAME` | Create a new branch |
| `bait branch delete NAME` | Delete a branch |
| `bait switch NAME [-c]` | Switch to a branch (create with `-c`) |
| `bait clean [--except BRANCHES] [--yes]` | Prune unneeded branches |

### Merging

| Command | Purpose |
|---------|---------|
| `bait merge BRANCH` | Merge BRANCH into current branch |

Merges are always recorded as new commits (no fast-forward / rebase). Conflicts are marked with `<<<<<<<`, `=======`, `>>>>>>>` and can be saved with or without resolution.

### Collaboration

| Command | Purpose |
|---------|---------|
| `bait review create [--reviewer NAME] [--message TEXT]` | Create a review request |
| `bait review list` | List open reviews |
| `bait issue create --title TEXT [--body TEXT] [--labels ...]` | Create an issue |
| `bait issue list` | List issues |
| `bait pull-request create --base BRANCH [--title TEXT] [--body TEXT]` | Create a pull request |
| `bait pull-request list` | List pull requests |

### Remotes

| Command | Purpose |
|---------|---------|
| `bait remote add NAME URL` | Add a remote repository |
| `bait remote list` | List configured remotes |
| `bait remote remove NAME` | Remove a remote |
| `bait sync [--local PATH] [--remote NAME]` | Sync with a remote or local clone (push + pull objects and refs) |

### BAIT Server (Docker)

Run a self-hosted BAIT remote server with Docker Compose:

```bash
mkdir -p remote-repo
bait init remote-repo
docker compose -f docker-compose.server.yml up --build -d
```

Add and sync from a local repo:

```bash
bait remote add origin http://127.0.0.1:7979
bait sync --remote origin
```

What `bait sync --remote` does:
- Pushes local objects missing on remote
- Pushes local branch refs to remote
- Pulls remote objects missing locally
- Pulls remote refs into `.bait/refs/remotes/<name>/...`

Server API highlights:
- `GET /health`
- `GET /objects/list`
- `GET /objects/:hash`
- `POST /objects`
- `GET /refs/heads`
- `GET|POST /refs/heads/:branch`
- `GET /branches`
- `GET /branches/:branch/log?limit=50`
- `GET /commits/:hash`
- `GET /commits/:hash/tree`

### Hooks

| Command | Purpose |
|---------|---------|
| `bait hooks list` | List installed hooks |
| `bait hooks install-sample NAME` | Install a sample hook script |

### Storage

| Command | Purpose |
|---------|---------|
| `bait storage stats` | Show object-store and index footprint statistics |

### AI Index

| Command | Purpose |
|---------|---------|
| `bait ai index [--rebuild]` | Build or refresh the lightweight AI symbol index |
| `bait ai find <QUERY> [--prefix]` | Look up symbols by exact or prefix match |
| `bait ai modules` | List indexed modules and exported symbol counts |

The default AI index is intentionally small. It stores symbol names, kinds, file/line locations, module names, visibility, and short doc summaries under `.bait/ai-index/`.

The index is refreshed automatically after `bait save`, `bait switch`, and `bait sync`, and can be rebuilt manually at any time.

### AI Benchmark Harness

To benchmark indexed BAIT symbol lookup against a Git-style search workflow:

```bash
chmod +x scripts/bench_ai_symbol_lookup.sh
scripts/bench_ai_symbol_lookup.sh /tmp/vcs-large/bait-repo /tmp/vcs-large/git-repo Repository 10
```

This compares:
- `bait ai find <symbol>` on a prebuilt BAIT symbol index
- `git grep` over the equivalent Git repository

Use this harness to quantify whether AI-oriented retrieval is faster with BAIT's default page index than with repeated full-tree Git search.

### Multi-Repo AI Benchmark Suite

For demos and comparisons across several repository shapes, run the suite script:

```bash
chmod +x scripts/bench_ai_agent_suite.sh
scripts/bench_ai_agent_suite.sh 5
```

This generates three representative repo pairs under `/tmp/bait-ai-suite/` and benchmarks each pair separately:
- small code-heavy repo
- docs-heavy repo
- large mixed repo

The goal is to show the same BAIt advantage across different shapes of codebases, not just a single cherry-picked repository.

### BAIt Cloud Services

The product itself stays open and local-first. Monetization lives in the service layer:
- hosted BAIt repositories with private org spaces
- managed remote sync and backup
- pull request and issue hosting
- AI indexing and search at scale
- review automation and policy gates
- enterprise support, SLAs, and onboarding

To showcase both latency and estimated token-load savings for AI agents:

```bash
chmod +x scripts/bench_ai_agent_savings.sh
scripts/bench_ai_agent_savings.sh /tmp/vcs-large/bait-repo /tmp/vcs-large/git-repo 10 EchoRunner Repository
```

This script reports, per query:
- average lookup time for `bait ai find` and `git grep`
- average output bytes returned to the caller
- estimated tokens (`ceil(bytes/4)`) as a practical proxy for LLM context pressure
- percentage token reduction and speedup multiplier

## Architecture

```
.bait/
├── objects/        # Content-addressed object store (zstd-compressed)
│   ├── ab/
│   │   └── cdef0...  # Object data
│   └── ...
├── refs/
│   ├── heads/      # Local branch pointers
│   │   ├── main
│   │   └── dev
│   └── remotes/    # Remote tracking branches
├── HEAD            # Current branch reference
├── config          # Repository config (TOML)
├── index           # Staging area (bincode)
└── oplog/
    └── ops         # Operation log (ndjson) for undo
```

### Objects

Three types of objects, all serialized with bincode and compressed with zstd:

**Blob**
```rust
struct Blob {
    data: Vec<u8>,
}
```

**Tree** (directory snapshot)
```rust
struct TreeEntry {
    name: String,           // Single path component
    hash: Hash,             // Content hash
    is_dir: bool,
    executable: bool,       // Unix executable bit
}

struct Tree {
    entries: Vec<TreeEntry>,  // Sorted by name
}
```

**Commit**
```rust
struct Commit {
    tree: Hash,                    // Root tree
    parents: Vec<Hash>,            // 0 = root, 1 = normal, 2+ = merge
    rushi: String,                 // Author name
    email: String,                 // Author email
    narada: Option<String>,        // Reviewer name
    narada_email: Option<String>,  // Reviewer email
    timestamp: i64,                // Unix seconds
    message: String,               // Commit message
    has_conflicts: bool,           // Unresolved merge conflicts
}
```

## Multilingual Support

BAIT is designed for global audiences. All CLI messages are translated at runtime based on the `LANG` environment variable.

Supported languages:
- 🇬🇧 English
- 🇮🇳 Hindi (हिन्दी)
- 🇮🇳 Tamil (தமிழ்)
- 🇮🇳 Bengali (বাংলা)
- 🇮🇳 Telugu (తెలుగు)

### Using BAIT in Hindi

```bash
export LANG=hi_IN.UTF-8
bait status
```

Output:
```
On branch main

बिना स्टेज के बदलाव:
  बदला गया:   README.md

अनट्रैक्ड फ़ाइलें ('bait add' से स्टेज करें):
  feature.txt
```

All message strings are embedded in the binary at compile time for portability.

## AI Index — How It Works

The AI index is the feature that sets BAIT apart for AI-assisted development. It lives at `.bait/ai-index/symbols.idx` — a compact, zstd-compressed binary index of every symbol in your codebase.

### What gets indexed

| Field | Example |
|-------|---------|
| Symbol name | `Repository`, `cmd_save`, `AiIndex` |
| Kind | `Struct`, `Function`, `Enum`, `Trait`, `Impl`, `Const` |
| File + line | `crates/bait-core/src/repo.rs:12` |
| Module path | `crates::bait_core::repo` |
| Visibility | public / private |
| Doc summary | first 160 chars of `///` doc comment |

Supported languages: **Rust, TypeScript, JavaScript, Python, Java, Go, C/C++, C#, Ruby, PHP, Swift, Kotlin, Scala, Shell**.

### Incremental refresh

The index uses `(size, mtime_ns)` fingerprints to only re-parse **changed files**. Refresh is automatic after every `bait save`, `bait switch`, and `bait sync`. Rebuild manually at any time:

```bash
bait ai index           # refresh (incremental)
bait ai index --rebuild # drop and rebuild from scratch
```

### Querying

```bash
bait ai find Repository          # exact match
bait ai find Repo --prefix       # prefix match
bait ai modules                  # list modules + export counts
```

Output is structured plain text — minimal bytes, no raw file contents. This is what makes it fast and token-efficient for AI agents.

### Why it matters for AI agents

When an AI agent needs to understand your codebase it can call `bait ai find <symbol>` and get a precise, structured answer in ~1 ms instead of scanning thousands of lines with `git grep`. Benchmarks across three repo shapes show:

| Metric | BAIT AI index | git grep |
|--------|:---:|:---:|
| Avg lookup time | ~1 ms | ~50–200 ms |
| Output bytes per query | ~200 B | ~15–40 KB |
| Estimated tokens | ~50 | ~4 000–10 000 |
| Token reduction | — | **95–99%** |

---

## Using BAIT AI Index in Agentic Mode

### GitHub Copilot (VS Code)

Add BAIT as an MCP-style context source in your workspace `.vscode/settings.json`:

```json
{
  "github.copilot.chat.codeGeneration.instructions": [
    {
      "text": "This repo uses BAIT for version control. To look up any symbol, run: bait ai find <symbol> --prefix. To list all modules: bait ai modules. Prefer these commands over reading raw source files."
    }
  ]
}
```

Then in any Copilot chat, agent mode, or inline chat you can ask:
> _"Find where Repository is defined"_ → Copilot will call `bait ai find Repository`

**For agent/edit mode**, add a `.github/copilot-instructions.md`:

```markdown
## Symbol lookup
Always use `bait ai find <name>` before reading files. It returns file, line,
module, visibility, and doc summary with minimal token cost.

## Module overview
Run `bait ai modules` to get a map of all modules and their exports.
```

### Cursor

Add a `.cursorrules` file (or `cursor.rules` in the project root):

```
# BAIT AI Index
Before exploring this codebase, always run:
  bait ai find <symbol>      - to locate a specific symbol
  bait ai find <prefix> --prefix  - for partial matches
  bait ai modules            - to understand the module structure

Do NOT use grep or read entire files when bait ai find can answer the question.
The index returns: name, kind, file, line, module, visibility, doc summary.
```

Cursor's agent will pick this up automatically in every session.

### Claude (claude.ai / Claude Desktop with MCP)

Add a `CLAUDE.md` in your repo root:

```markdown
## Code navigation
This repo is tracked with BAIT. Use these shell commands for symbol lookup:

```shell
# Find a symbol by exact name
bait ai find <symbol>

# Find by prefix
bait ai find <prefix> --prefix

# List all modules and export counts
bait ai modules
```

These commands are far cheaper in tokens than reading files directly.
Always try `bait ai find` first.
```

In Claude Desktop with MCP, you can also expose BAIT as a tool in `mcp-config.json`:

```json
{
  "tools": [
    {
      "name": "bait_find",
      "description": "Find a code symbol in the BAIT AI index. Returns name, kind, file, line, module, and doc summary.",
      "command": "bait",
      "args": ["ai", "find", "{query}", "--prefix"]
    },
    {
      "name": "bait_modules",
      "description": "List all indexed modules and their exported symbol counts.",
      "command": "bait",
      "args": ["ai", "modules"]
    }
  ]
}
```

### Any agent / custom tool

`bait ai find` exits `0` and writes to stdout. JSON output coming soon. Until then, stdout is one symbol per line in the format:

```
[Function] cmd_save  crates/bait-cli/src/main.rs:620  (pub)  crates::bait_cli::main
```

Parse with:
```bash
bait ai find Repository | awk '{print $2, $3}'
```

---

## Development

### Building

```bash
cargo build --workspace
```

### Running Tests

```bash
cargo test --workspace
```

### Running the Smoke Test

```bash
cd /tmp && mkdir smoke-repo && cd smoke-repo
bait init
echo "hello" > hello.txt
bait add hello.txt
bait save --message "first"
bait log
```

### CI/CD

Automated testing runs on every push to main/dev:

- Build on Linux, macOS, Windows
- Cargo clippy linting
- Unit tests
- Smoke test (init → add → save → log)
- Multilingual smoke test (LANG=hi_IN)

See [`.github/workflows/ci.yml`](.github/workflows/ci.yml) for details.

## Contributing

We welcome contributions in any language and from all backgrounds. Please:

1. **Fork the repository** and create a feature branch
2. **Add tests** for new functionality
3. **Follow the code style** (cargo fmt, cargo clippy)
4. **Document multilingual implications** — test i18n strings when adding new messages
5. **Submit a pull request** with a clear description

### Adding a New Language

To add a new language (e.g., Spanish):

1. Create `crates/bait-cli/i18n/es.toml` with all keys from `en.toml`
2. Update `crates/bait-cli/src/locale.rs` to detect the language
3. Test with `LANG=es_ES.UTF-8 bait status`
4. Update this README

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE) for details.

---

## FAQ

### Q: Why "BAIT"?

**A:** Bharat (भारत) — India — combined with "Autonomous Integrated Tracker." The name reflects the project's origin and its mission to be accessible globally, especially in India and South Asia.

### Q: How is BAIT different from Git?

**A:**
- **Simpler branching** — no rebasing, no detached HEAD state
- **Integrated code review** — narada (reviewer) is a first-class concept
- **Multilingual by design** — Hindi, Tamil, Bengali, Telugu, English
- **Undo everything** — full operation log enables reversal
- **Content-addressed** — BLAKE3 instead of SHA-1, zstd compression

### Q: Can I use BAIT for production?

**A:** BAIT is stable for local use and small teams. Remote sync, issue tracking, and pull requests are fully functional. Large-scale multi-user hosting is on the roadmap via BAIt Cloud.

### Q: Does BAIT work on Windows?

**A:** The CLI and core library work on Windows. Pre-built binaries coming soon.

### Q: Can I migrate from Git to BAIT?

**A:** A migration tool (`bait migrate`) is planned. For now, you can initialize a new BAIT repository and port history manually.

### Q: Is there a web UI?

**A:** A Leptos-based web UI is in development. For now use the CLI and the HTTP API (`bait-server`).

### Q: How do I sync with others?

**A:** Peer-to-peer local sync: `bait sync --local /path/to/other/repo`. HTTP remote sync: run `bait-server`, add a remote with `bait remote add origin <url>`, then `bait sync --remote origin`. Set `BAIT_TOKEN` on the server to enable bearer-token auth.

### Q: How do I use the AI index with GitHub Copilot?

**A:** Add a `.github/copilot-instructions.md` telling Copilot to call `bait ai find <symbol>` before reading files. See the [Using BAIT AI Index in Agentic Mode](#using-bait-ai-index-in-agentic-mode) section above for step-by-step instructions for Copilot, Cursor, and Claude.

---

## Support

- 📖 **Documentation** — See [docs/](docs/) (coming soon)
- 🐛 **Bug Reports** — Open an issue on GitHub
- 💬 **Discussions** — GitHub Discussions (coming soon)
- 📧 **Email** — bait@example.com (coming soon)

---

**BAIT: Making version control simple, clear, and global.**
