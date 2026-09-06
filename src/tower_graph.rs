use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use crate::config::Config;
use crate::error::SlopError;
use crate::graph::find_git_root;
use crate::graphstore::{self, Tier, TowerGraph, render};
use crate::models::CliArgs;
use crate::pathing::{
    collect_source_files_reporting_with_slopignore, resolve_absolute, resolve_output_dir,
    should_respect_gitignore,
};

pub fn resolve_tower_seeds(
    args: &CliArgs,
    config: &Config,
) -> Result<(PathBuf, Vec<String>), SlopError> {
    let cwd = std::env::current_dir().map_err(|source| SlopError::FileReadFailure {
        path: PathBuf::from("."),
        source,
    })?;
    let inputs = args
        .inputs
        .iter()
        .map(|path| resolve_absolute(path, &cwd))
        .collect::<Result<Vec<_>, _>>()?;
    let mut roots = BTreeSet::new();
    for input in &inputs {
        if !input.exists() {
            return Err(SlopError::MissingInputPath(input.clone()));
        }
        let anchor = if input.is_dir() {
            input.as_path()
        } else {
            input.parent().unwrap_or(input)
        };
        roots.insert(
            find_git_root(anchor)
                .ok_or_else(|| SlopError::GraphRepoRootUnresolved(input.clone()))?,
        );
    }
    if roots.len() != 1 {
        return Err(SlopError::TowerSeedsSpanMultipleRepos(
            roots.into_iter().collect(),
        ));
    }
    let repo_root = roots.into_iter().next().expect("one root");
    let max_depth = if args.recursive {
        Some(usize::MAX)
    } else {
        Some(0)
    };
    let walk = collect_source_files_reporting_with_slopignore(
        &inputs,
        max_depth,
        &[],
        should_respect_gitignore(false, config),
        false,
    )?;
    let (project, _) = graphstore::refresh_project_graph(&repo_root, config, args.reindex)?;
    let mut seeds = BTreeSet::new();
    for path in walk.files {
        let rel = path
            .strip_prefix(&repo_root)
            .map_err(|_| SlopError::TowerSeedOutsideRepo(path.clone()))?
            .to_string_lossy()
            .replace('\\', "/");
        if project.file(&rel).is_some() {
            seeds.insert(rel);
        }
    }
    if seeds.is_empty() {
        return Err(SlopError::TowerSeedSetEmpty);
    }
    Ok((repo_root, seeds.into_iter().collect()))
}

pub fn run_tower_graph(args: &CliArgs, config: &Config) -> Result<Vec<PathBuf>, SlopError> {
    let (root, seeds) = resolve_tower_seeds(args, config)?;
    let tower = graphstore::refresh_tower_graph(&root, &seeds, config, args.reindex)?;
    if !args.silent {
        print_summary(&tower);
    }
    if !config.graph_emit_artifact {
        return Ok(Vec::new());
    }
    let cwd = std::env::current_dir().map_err(|source| SlopError::FileReadFailure {
        path: PathBuf::from("."),
        source,
    })?;
    let dir = resolve_output_dir(
        args.output_dir
            .as_deref()
            .or(args.slop_to.as_deref())
            .or(config.slopified_folder.as_deref()),
        &cwd,
    )?;
    fs::create_dir_all(&dir).map_err(|source| SlopError::DirectoryCreationFailure {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join(format!(
        "{}.tower-graph.{}.md",
        tower.repo_id,
        &tower.seed_digest[..8]
    ));
    let (project, _) = graphstore::refresh_project_graph(&root, config, false)?;
    fs::write(&path, render::render_tower(&tower, &project)).map_err(|source| {
        SlopError::FileWriteFailure {
            path: path.clone(),
            source,
        }
    })?;
    Ok(vec![path])
}

fn print_summary(tower: &TowerGraph) {
    let count = |tier| {
        tower
            .members
            .iter()
            .filter(|member| member.tier == tier)
            .count()
    };
    eprintln!(
        "tower: {} tier-0, {} tier-1, {} tier-2, {} tier-3",
        count(Tier::Zero),
        count(Tier::One),
        count(Tier::Two),
        count(Tier::Three)
    );
    for member in tower
        .members
        .iter()
        .filter(|member| member.tier == Tier::One)
        .take(3)
    {
        eprintln!("  {:.6} {}", member.score, member.rel);
    }
}
