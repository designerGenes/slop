//! Where graphs live and how they get there intact.
//!
//! The canonical copy is a cache artifact under `$HOME/.cache/slop/graphs/`,
//! keyed by a hash of the repository's absolute path so two checkouts of the
//! same project never collide and a moved checkout simply rebuilds. Nothing is
//! written inside the repository: a generated graph in the working tree is one
//! more thing to gitignore and one more way to dirty a diff.

use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::SlopError;

use super::model::{PROJECT_GRAPH_SCHEMA, ProjectGraph};
use super::tower::{TOWER_GRAPH_SCHEMA, TowerGraph};

/// Stable identifier for a repository: readable prefix plus a path digest.
///
/// The digest is over the canonicalized path, so `/repo` and `/repo/` and a
/// symlinked route to the same directory all land on one entry.
pub fn repo_id(repo_root: &Path) -> String {
    let canonical = fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let digest = blake3::hash(canonical.to_string_lossy().as_bytes())
        .to_hex()
        .to_string();
    let name = canonical
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    let slug: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    format!("{}-{}", slug.trim_matches('-'), &digest[..16])
}

/// Root of the graph store: config override, else `$HOME/.cache/slop/graphs`.
///
/// This sits beside the selection index rather than under `$HOME/.slop/`
/// because both are derived data that a cache cleaner may delete without
/// consequence; `$HOME/.slop/` holds things whose loss would cost work.
pub fn resolve_graph_dir(config: &Config) -> PathBuf {
    config
        .graph_store_dir
        .as_deref()
        .map(crate::pathing::expand_tilde)
        .or_else(crate::config::default_graph_store_dir)
        .unwrap_or_else(|| PathBuf::from(".slop-graphs"))
}

pub fn repo_graph_dir(config: &Config, repo_root: &Path) -> PathBuf {
    resolve_graph_dir(config).join(repo_id(repo_root))
}

pub fn project_graph_path(config: &Config, repo_root: &Path) -> PathBuf {
    repo_graph_dir(config, repo_root).join("project.json")
}

pub fn tower_graph_dir(config: &Config, repo_root: &Path) -> PathBuf {
    repo_graph_dir(config, repo_root).join("towers")
}
pub fn tower_graph_path(config: &Config, repo_root: &Path, seed_digest: &str) -> PathBuf {
    tower_graph_dir(config, repo_root).join(format!("{seed_digest}.json"))
}
pub fn seed_digest(seeds: &[String]) -> String {
    let mut sorted: Vec<&str> = seeds.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    blake3::hash(sorted.join("\n").as_bytes())
        .to_hex()
        .to_string()
}

/// Load a stored graph, or `None` when there is nothing usable to load.
///
/// Every failure mode — absent, unreadable, corrupt, stale schema — collapses
/// to `None`, because the only correct response to any of them is the same: do
/// a cold build. Returning an error here would force every caller to translate
/// "your cache is stale" into "rebuild it", which is not a decision worth
/// distributing.
pub fn load_project_graph(path: &Path) -> Option<ProjectGraph> {
    let raw = fs::read_to_string(path).ok()?;
    let graph: ProjectGraph = serde_json::from_str(&raw).ok()?;
    if graph.schema != PROJECT_GRAPH_SCHEMA {
        return None;
    }
    Some(graph)
}

/// Write a graph atomically: full write to a sibling temp file, then rename.
///
/// A half-written graph that still parses is worse than no graph at all, since
/// the loader would accept it and the incremental path would trust its hashes.
/// Rename within a directory is atomic on every platform slop targets.
pub fn save_project_graph(path: &Path, graph: &ProjectGraph) -> Result<(), SlopError> {
    save_json(path, graph, "serialize graph")
}

pub fn load_tower_graph(path: &Path) -> Option<TowerGraph> {
    let raw = fs::read_to_string(path).ok()?;
    let tower: TowerGraph = serde_json::from_str(&raw).ok()?;
    (tower.schema == TOWER_GRAPH_SCHEMA).then_some(tower)
}

pub fn save_tower_graph(path: &Path, tower: &TowerGraph) -> Result<(), SlopError> {
    save_json(path, tower, "serialize tower graph")
}

fn save_json<T: serde::Serialize>(path: &Path, value: &T, label: &str) -> Result<(), SlopError> {
    let parent = path.parent().ok_or_else(|| {
        SlopError::GraphStoreFailure(format!("{} has no parent directory", path.display()))
    })?;
    fs::create_dir_all(parent).map_err(|error| SlopError::DirectoryCreationFailure {
        path: parent.to_path_buf(),
        source: error,
    })?;

    let serialized = serde_json::to_string(value)
        .map_err(|error| SlopError::GraphStoreFailure(format!("{label}: {error}")))?;

    let temp = path.with_extension("json.tmp");
    fs::write(&temp, serialized).map_err(|error| SlopError::FileWriteFailure {
        path: temp.clone(),
        source: error,
    })?;
    fs::rename(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        SlopError::FileWriteFailure {
            path: path.to_path_buf(),
            source: error,
        }
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphstore::model::{GraphStats, Structure};

    fn empty_graph(root: &Path) -> ProjectGraph {
        ProjectGraph {
            schema: PROJECT_GRAPH_SCHEMA,
            repo_root: root.to_string_lossy().to_string(),
            repo_id: repo_id(root),
            generated_at_unix: 0,
            generator_version: "test".to_string(),
            files: Vec::new(),
            symbol_edges: Vec::new(),
            cochange_edges: Vec::new(),
            communities: Vec::new(),
            structure: Structure::default(),
            stats: GraphStats::default(),
        }
    }

    #[test]
    fn repo_id_is_stable_and_path_specific() {
        let a = tempfile::tempdir().expect("tempdir");
        let b = tempfile::tempdir().expect("tempdir");
        assert_eq!(repo_id(a.path()), repo_id(a.path()));
        assert_ne!(repo_id(a.path()), repo_id(b.path()));
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("project.json");
        let graph = empty_graph(dir.path());
        save_project_graph(&path, &graph).expect("save");
        let loaded = load_project_graph(&path).expect("load");
        assert_eq!(loaded.repo_id, graph.repo_id);
    }

    #[test]
    fn a_stale_schema_reads_as_no_graph_at_all() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("project.json");
        let mut graph = empty_graph(dir.path());
        graph.schema = PROJECT_GRAPH_SCHEMA + 1;
        save_project_graph(&path, &graph).expect("save");
        assert!(load_project_graph(&path).is_none());
    }

    #[test]
    fn corrupt_json_reads_as_no_graph_rather_than_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("project.json");
        fs::write(&path, "{ not json").expect("write");
        assert!(load_project_graph(&path).is_none());
    }

    #[test]
    fn saving_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("project.json");
        save_project_graph(&path, &empty_graph(dir.path())).expect("save");
        assert!(!dir.path().join("project.json.tmp").exists());
    }
}
