use anyhow::Result;
use axum::{
    extract::{Path, Query, State},
    http::{Request, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{get, post},
    Json,
    Router,
};
use bait_core::{
    collab::{issues_dir, pull_requests_dir, IssueRecord, PullRequestRecord},
    objects::{Commit, Hash, Tree},
    repo::Repository,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    repo_path: Arc<PathBuf>,
    /// Optional bearer token. When set every non-/health request must carry
    /// `Authorization: Bearer <token>`.
    auth_token: Option<Arc<String>>,
}

// ── Auth middleware ───────────────────────────────────────────────────────────

async fn require_auth(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    if let Some(expected) = &state.auth_token {
        let provided = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        match provided {
            Some(token) if token == expected.as_str() => {}
            _ => return StatusCode::UNAUTHORIZED.into_response(),
        }
    }
    next.run(req).await
}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let repo_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let port: u16 = std::env::var("BAIT_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(7979);

    let auth_token = std::env::var("BAIT_TOKEN").ok().map(Arc::new);
    if auth_token.is_some() {
        println!("bait-server: bearer-token auth enabled (BAIT_TOKEN is set)");
    }

    let state = AppState {
        repo_path: Arc::new(repo_path.clone()),
        auth_token,
    };

    let protected = Router::new()
        .route("/objects/:hash", get(get_object))
        .route("/objects/list", get(list_objects))
        .route("/objects", post(post_object))
        .route("/refs/heads", get(list_refs))
        .route("/refs/heads/:branch", get(get_ref).post(set_ref))
        .route("/branches", get(list_branches))
        .route("/branches/:branch/log", get(get_branch_log))
        .route("/commits/:hash", get(get_commit))
        .route("/commits/:hash/tree", get(get_commit_tree))
        .route("/issues", get(list_issues).post(create_issue))
        .route("/pulls", get(list_pull_requests).post(create_pull_request))
        .layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let app = Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    println!("bait-server listening on http://{}", addr);
    println!("Serving repository: {}", repo_path.display());

    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn get_object(
    Path(hash_hex): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, vec![]).into_response(),
    };
    let hash = match Hash::from_hex(&hash_hex) {
        Ok(h) => h,
        Err(_) => return (StatusCode::BAD_REQUEST, vec![]).into_response(),
    };
    // Return the raw compressed bytes so the client can store them directly.
    let hex = hash.to_hex();
    let obj_path = repo.bait_dir.join("objects").join(&hex[..2]).join(&hex[2..]);
    match std::fs::read(&obj_path) {
        Ok(data) => (StatusCode::OK, data).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, vec![]).into_response(),
    }
}

async fn post_object(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    // Decompress and re-store so we get the correct hash path.
    let data = match zstd::decode_all(std::io::Cursor::new(&body)) {
        Ok(d) => d,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    match repo.store.write(&data) {
        Ok(_) => StatusCode::CREATED,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn list_refs(State(state): State<AppState>) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, String::new()).into_response(),
    };
    let branches = repo.branches.list().unwrap_or_default();
    let mut lines = Vec::new();
    for b in branches {
        if let Ok(Some(h)) = repo.branches.read(&b) {
            lines.push(format!("{}\t{}", h.to_hex(), b));
        }
    }
    (StatusCode::OK, lines.join("\n")).into_response()
}

#[derive(Debug, Serialize)]
struct BranchInfo {
    name: String,
    head: Option<String>,
}

async fn list_branches(State(state): State<AppState>) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<BranchInfo>::new())),
    };

    let mut out = Vec::new();
    for b in repo.branches.list().unwrap_or_default() {
        let head = repo
            .branches
            .read(&b)
            .ok()
            .and_then(|h| h.map(|x| x.to_hex()));
        out.push(BranchInfo { name: b, head });
    }
    (StatusCode::OK, Json(out))
}

async fn list_objects(State(state): State<AppState>) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, String::new()).into_response(),
    };
    let mut hashes = Vec::new();
    let objects_dir = repo.bait_dir.join("objects");
    if let Ok(shards) = std::fs::read_dir(objects_dir) {
        for shard in shards.filter_map(|e| e.ok()) {
            if !shard.path().is_dir() {
                continue;
            }
            let shard_name = shard.file_name().to_string_lossy().into_owned();
            if let Ok(entries) = std::fs::read_dir(shard.path()) {
                for obj in entries.filter_map(|e| e.ok()) {
                    if obj.path().is_file() {
                        let tail = obj.file_name().to_string_lossy().into_owned();
                        if shard_name.len() == 2 && tail.len() == 62 {
                            hashes.push(format!("{}{}", shard_name, tail));
                        }
                    }
                }
            }
        }
    }
    hashes.sort();
    (StatusCode::OK, hashes.join("\n")).into_response()
}

#[derive(Debug, Deserialize)]
struct IssueCreateRequest {
    title: String,
    body: Option<String>,
    labels: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PullRequestCreateRequest {
    title: Option<String>,
    body: Option<String>,
    head_branch: String,
    base_branch: String,
    reviewer: Option<String>,
    reviewer_email: Option<String>,
}

fn records_dir(path: &PathBuf) -> PathBuf {
    path.clone()
}

fn load_json_records<T: DeserializeOwned>(dir: &PathBuf) -> Result<Vec<T>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.path().extension().map_or(false, |ext| ext == "json") {
            entries.push(entry.path());
        }
    }
    entries.sort();

    let mut out = Vec::new();
    for path in entries {
        let raw = std::fs::read_to_string(path)?;
        out.push(serde_json::from_str(&raw)?);
    }
    Ok(out)
}

fn next_record_id(dir: &PathBuf) -> Result<usize> {
    if !dir.exists() {
        return Ok(1);
    }
    Ok(std::fs::read_dir(dir)?.filter_map(|e| e.ok()).filter(|e| e.path().extension().map_or(false, |ext| ext == "json")).count() + 1)
}

fn save_json_record<T: Serialize>(dir: &PathBuf, id: usize, value: &T) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{:04}.json", id));
    std::fs::write(path, serde_json::to_string_pretty(value)?)?;
    Ok(())
}

async fn list_issues(State(state): State<AppState>) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<IssueRecord>::new())),
    };
    let dir = records_dir(&issues_dir(&repo.bait_dir));
    let mut issues = load_json_records::<IssueRecord>(&dir).unwrap_or_default();
    issues.sort_by(|a, b| a.id.cmp(&b.id));
    (StatusCode::OK, Json(issues))
}

async fn create_issue(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<IssueCreateRequest>,
) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    let dir = records_dir(&issues_dir(&repo.bait_dir));
    let id = match next_record_id(&dir) {
        Ok(id) => id,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    let record = IssueRecord {
        id,
        title: body.title,
        body: body.body,
        labels: body.labels,
        status: "open".to_string(),
        author: repo.config.user.name.clone(),
        author_email: repo.config.user.email.clone(),
        branch: repo.current_branch().unwrap_or_else(|_| "main".to_string()),
        commit: repo.head_commit().ok().flatten().map(|h| h.to_hex()).unwrap_or_default(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    match save_json_record(&dir, id, &record) {
        Ok(_) => StatusCode::CREATED,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn list_pull_requests(State(state): State<AppState>) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<PullRequestRecord>::new())),
    };
    let dir = records_dir(&pull_requests_dir(&repo.bait_dir));
    let mut pulls = load_json_records::<PullRequestRecord>(&dir).unwrap_or_default();
    pulls.sort_by(|a, b| a.id.cmp(&b.id));
    (StatusCode::OK, Json(pulls))
}

async fn create_pull_request(
    State(state): State<AppState>,
    axum::Json(body): axum::Json<PullRequestCreateRequest>,
) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    if !repo.branches.exists(&body.base_branch) {
        return StatusCode::BAD_REQUEST;
    }
    let dir = records_dir(&pull_requests_dir(&repo.bait_dir));
    let id = match next_record_id(&dir) {
        Ok(id) => id,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    let record = PullRequestRecord {
        id,
        title: body.title.unwrap_or_else(|| format!("Merge {} into {}", body.head_branch, body.base_branch)),
        body: body.body,
        head_branch: body.head_branch,
        base_branch: body.base_branch,
        head_commit: repo.head_commit().ok().flatten().map(|h| h.to_hex()).unwrap_or_default(),
        status: "open".to_string(),
        author: repo.config.user.name.clone(),
        author_email: repo.config.user.email.clone(),
        reviewer: body.reviewer,
        reviewer_email: body.reviewer_email,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    match save_json_record(&dir, id, &record) {
        Ok(_) => StatusCode::CREATED,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

async fn get_ref(
    Path(branch): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, String::new()).into_response(),
    };
    match repo.branches.read(&branch) {
        Ok(Some(h)) => (StatusCode::OK, h.to_hex()).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, String::new()).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, String::new()).into_response(),
    }
}

#[derive(Deserialize)]
struct SetRefBody {
    hash: String,
}

async fn set_ref(
    Path(branch): Path<String>,
    State(state): State<AppState>,
    axum::Json(body): axum::Json<SetRefBody>,
) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR,
    };
    let hash = match Hash::from_hex(&body.hash) {
        Ok(h) => h,
        Err(_) => return StatusCode::BAD_REQUEST,
    };
    match repo.branches.write(&branch, &hash) {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[derive(Debug, Deserialize)]
struct LogQuery {
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct CommitInfo {
    hash: String,
    tree: String,
    parents: Vec<String>,
    rushi: String,
    email: String,
    narada: Option<String>,
    narada_email: Option<String>,
    timestamp: i64,
    message: String,
    has_conflicts: bool,
}

async fn get_commit(
    Path(hash_hex): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(None::<CommitInfo>)),
    };

    let hash = match Hash::from_hex(&hash_hex) {
        Ok(h) => h,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(None::<CommitInfo>)),
    };

    match repo.read_commit(&hash) {
        Ok(c) => {
            let info = CommitInfo {
                hash: hash.to_hex(),
                tree: c.tree.to_hex(),
                parents: c.parents.into_iter().map(|p| p.to_hex()).collect(),
                rushi: c.rushi,
                email: c.email,
                narada: c.narada,
                narada_email: c.narada_email,
                timestamp: c.timestamp,
                message: c.message,
                has_conflicts: c.has_conflicts,
            };
            (StatusCode::OK, Json(Some(info)))
        }
        Err(_) => (StatusCode::NOT_FOUND, Json(None::<CommitInfo>)),
    }
}

#[derive(Debug, Serialize)]
struct TreeNode {
    path: String,
    hash: String,
    is_dir: bool,
    executable: bool,
}

async fn get_commit_tree(
    Path(hash_hex): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<TreeNode>::new())),
    };
    let hash = match Hash::from_hex(&hash_hex) {
        Ok(h) => h,
        Err(_) => return (StatusCode::BAD_REQUEST, Json(Vec::<TreeNode>::new())),
    };
    let commit = match repo.read_commit(&hash) {
        Ok(c) => c,
        Err(_) => return (StatusCode::NOT_FOUND, Json(Vec::<TreeNode>::new())),
    };

    let mut out = Vec::new();
    if flatten_tree_nodes(&repo, &commit.tree, "", &mut out).is_err() {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<TreeNode>::new()));
    }
    (StatusCode::OK, Json(out))
}

async fn get_branch_log(
    Path(branch): Path<String>,
    Query(q): Query<LogQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let repo = match Repository::open(&state.repo_path) {
        Ok(r) => r,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<CommitInfo>::new())),
    };

    let mut current = match repo.branches.read(&branch) {
        Ok(Some(h)) => Some(h),
        Ok(None) => return (StatusCode::NOT_FOUND, Json(Vec::<CommitInfo>::new())),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(Vec::<CommitInfo>::new())),
    };

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let limit = q.limit.unwrap_or(20).max(1).min(500);

    while let Some(h) = current {
        if out.len() >= limit {
            break;
        }
        let key = h.to_hex();
        if !seen.insert(key.clone()) {
            break;
        }

        let c: Commit = match repo.read_commit(&h) {
            Ok(c) => c,
            Err(_) => break,
        };

        let parents_hex: Vec<String> = c.parents.iter().map(|p| p.to_hex()).collect();
        out.push(CommitInfo {
            hash: key,
            tree: c.tree.to_hex(),
            parents: parents_hex,
            rushi: c.rushi,
            email: c.email,
            narada: c.narada,
            narada_email: c.narada_email,
            timestamp: c.timestamp,
            message: c.message,
            has_conflicts: c.has_conflicts,
        });

        current = c.parents.into_iter().next();
    }

    (StatusCode::OK, Json(out))
}

fn flatten_tree_nodes(
    repo: &Repository,
    tree_hash: &Hash,
    prefix: &str,
    out: &mut Vec<TreeNode>,
) -> Result<()> {
    let tree: Tree = repo.read_tree(tree_hash)?;
    let mut dirs: BTreeMap<String, Hash> = BTreeMap::new();

    for e in tree.entries {
        let path = if prefix.is_empty() {
            e.name.clone()
        } else {
            format!("{}/{}", prefix, e.name)
        };
        out.push(TreeNode {
            path: path.clone(),
            hash: e.hash.to_hex(),
            is_dir: e.is_dir,
            executable: e.executable,
        });
        if e.is_dir {
            dirs.insert(path, e.hash);
        }
    }

    for (sub_path, sub_hash) in dirs {
        flatten_tree_nodes(repo, &sub_hash, &sub_path, out)?;
    }

    Ok(())
}
