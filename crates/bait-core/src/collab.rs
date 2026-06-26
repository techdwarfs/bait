use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueRecord {
    pub id: usize,
    pub title: String,
    pub body: Option<String>,
    pub labels: Vec<String>,
    pub status: String,
    pub author: String,
    pub author_email: String,
    pub branch: String,
    pub commit: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestRecord {
    pub id: usize,
    pub title: String,
    pub body: Option<String>,
    pub head_branch: String,
    pub base_branch: String,
    pub head_commit: String,
    pub status: String,
    pub author: String,
    pub author_email: String,
    pub reviewer: Option<String>,
    pub reviewer_email: Option<String>,
    pub created_at: String,
}

pub fn issues_dir(bait_dir: &Path) -> PathBuf {
    bait_dir.join("issues")
}

pub fn pull_requests_dir(bait_dir: &Path) -> PathBuf {
    bait_dir.join("pull-requests")
}
