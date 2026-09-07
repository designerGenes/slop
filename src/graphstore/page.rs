use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::SlopError;

use super::tower::Tier;

pub const PAGE_SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageStatus {
    Open,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PageAddReason {
    Opened,
    Requested,
    Promoted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageCloseSource {
    Direct,
    Returned,
    DirectAndReturned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageCloseChange {
    pub rel: String,
    pub source: PageCloseSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageFileState {
    pub rel: String,
    pub tier: Tier,
    pub base_sha: String,
    pub added_via: PageAddReason,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageManifest {
    pub schema: u32,
    pub page_id: String,
    pub repo_id: String,
    pub repo_root: String,
    pub task: Option<String>,
    pub status: PageStatus,
    pub seed_digest: String,
    pub opened_at_unix: u64,
    pub closed_at_unix: Option<u64>,
    pub files: Vec<PageFileState>,
    #[serde(default)]
    pub closed_changes: Vec<PageCloseChange>,
}

pub fn pages_dir(config: &Config) -> PathBuf {
    config
        .pages_dir
        .as_deref()
        .map(crate::pathing::expand_tilde)
        .or_else(crate::config::default_pages_dir)
        .unwrap_or_else(|| PathBuf::from(".slop-pages"))
}
pub fn page_dir(config: &Config, repo_id: &str, page_id: &str) -> PathBuf {
    pages_dir(config).join(repo_id).join(page_id)
}
pub fn manifest_path(config: &Config, repo_id: &str, page_id: &str) -> PathBuf {
    page_dir(config, repo_id, page_id).join("page.json")
}
pub fn context_path(config: &Config, repo_id: &str, page_id: &str) -> PathBuf {
    page_dir(config, repo_id, page_id).join("context.slop.md")
}

pub fn load_page(path: &Path) -> Option<PageManifest> {
    let body = fs::read_to_string(path).ok()?;
    let page: PageManifest = serde_json::from_str(&body).ok()?;
    (page.schema == PAGE_SCHEMA).then_some(page)
}

pub fn save_page(config: &Config, page: &PageManifest) -> Result<(), SlopError> {
    let path = manifest_path(config, &page.repo_id, &page.page_id);
    store_page_json(&path, page)
}

fn store_page_json(path: &Path, page: &PageManifest) -> Result<(), SlopError> {
    let parent = path.parent().expect("page manifest has parent");
    fs::create_dir_all(parent).map_err(|source| SlopError::DirectoryCreationFailure {
        path: parent.to_path_buf(),
        source,
    })?;
    let temp = path.with_extension("json.tmp");
    let body = serde_json::to_string(page)
        .map_err(|error| SlopError::GraphStoreFailure(format!("serialize page: {error}")))?;
    fs::write(&temp, body).map_err(|source| SlopError::FileWriteFailure {
        path: temp.clone(),
        source,
    })?;
    fs::rename(&temp, path).map_err(|source| SlopError::FileWriteFailure {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

pub fn open_pages(config: &Config, repo_id: &str) -> Vec<PageManifest> {
    let root = pages_dir(config).join(repo_id);
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut pages: Vec<PageManifest> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| load_page(&entry.path().join("page.json")))
        .filter(|page| page.status == PageStatus::Open)
        .collect();
    pages.sort_by(|a, b| a.page_id.cmp(&b.page_id));
    pages
}
