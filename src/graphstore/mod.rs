//! Persisted graphs: build once, refresh cheaply, reuse everywhere.
//!
//! Until now every graph slop produced was thrown away the moment the process
//! exited, and the next `-g` run re-parsed the entire repository to rebuild
//! something byte-identical. That was affordable when the graph was a garnish on
//! a slop file. It is not affordable when the graph is the thing that decides
//! which files an agent is allowed to look at, because then it is consulted many
//! times per task rather than once per bundle.
//!
//! So the graph becomes an artifact. It lives in a cache keyed by repository,
//! it carries the per-file hashes needed to know what changed, and it can be
//! read back by anything that needs to answer "what is near this file".
//!
//! What is here now (stage one) is the project graph: the whole repository, its
//! symbol edges, its co-change edges, its module structure. What comes next is
//! the tower graph, which is this graph queried from a set of seed files and
//! collapsed into tiers of attention.

pub mod build;
pub mod cochange;
pub mod community;
pub mod model;
pub mod page;
pub mod render;
pub mod store;
pub mod tower;

use std::path::Path;

use crate::config::Config;
use crate::error::SlopError;

pub use build::{BuildOptions, build_project_graph};
pub use model::{BuildReport, ProjectGraph};
pub use tower::{Tier, TowerGraph, TowerMember, build_tower_graph};

/// Bring the stored graph for `repo_root` up to date and persist it.
///
/// This is the entry point every command should use. It loads whatever is
/// cached, rebuilds only what changed, writes the result back atomically, and
/// hands back both the graph and an account of what the build actually did.
pub fn refresh_project_graph(
    repo_root: &Path,
    config: &Config,
    force_rebuild: bool,
) -> Result<(ProjectGraph, BuildReport), SlopError> {
    let path = store::project_graph_path(config, repo_root);
    let previous = if force_rebuild {
        None
    } else {
        store::load_project_graph(&path)
    };

    let options = BuildOptions::from_config(config, force_rebuild);
    let (graph, report) = build_project_graph(repo_root, previous.as_ref(), &options)?;
    store::save_project_graph(&path, &graph)?;

    Ok((graph, report))
}

/// Read the stored graph without touching the filesystem beyond the cache.
///
/// Callers that need an answer fast and can tolerate a slightly stale graph —
/// Stage 2's tower walk, once it exists — use this and let a later explicit
/// `--project-graph` run do the refreshing.
pub fn load_project_graph(repo_root: &Path, config: &Config) -> Option<ProjectGraph> {
    store::load_project_graph(&store::project_graph_path(config, repo_root))
}

pub fn refresh_tower_graph(
    repo_root: &Path,
    seeds: &[String],
    config: &Config,
    force_rebuild: bool,
) -> Result<TowerGraph, SlopError> {
    let (project, _) = refresh_project_graph(repo_root, config, force_rebuild)?;
    let digest = store::seed_digest(seeds);
    let path = store::tower_graph_path(config, repo_root, &digest);
    if !force_rebuild && let Some(tower) = store::load_tower_graph(&path) {
        if tower.project_graph_fingerprint == project.fingerprint() {
            return Ok(tower);
        }
    }
    let tower = tower::build_tower_graph(&project, seeds, config);
    store::save_tower_graph(&path, &tower)?;
    Ok(tower)
}
