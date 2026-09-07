use std::collections::{BTreeMap, BTreeSet};
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
        should_respect_gitignore(args.respect_gitignore, config),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierRecall {
    pub rel: String,
    pub tier: Option<Tier>,
}

/// Score known task files against a tower without involving an agent or
/// serializing a context page. Positional inputs are the seeds; --tier-recall
/// paths are the expected files.
pub fn run_tier_recall(args: &CliArgs, config: &Config) -> Result<(), SlopError> {
    let (root, seeds) = resolve_tower_seeds(args, config)?;
    let project = graphstore::load_project_graph(&root, config).ok_or_else(|| {
        SlopError::GraphStoreFailure(format!("project graph disappeared for {}", root.display()))
    })?;
    let tower = graphstore::build_tower_graph(&project, &seeds, config);
    let cwd = std::env::current_dir().map_err(|source| SlopError::FileReadFailure {
        path: PathBuf::from("."),
        source,
    })?;
    let mut targets = Vec::with_capacity(args.tier_recall.len());
    for target in &args.tier_recall {
        let target = resolve_absolute(target, &cwd)?;
        let rel = target
            .strip_prefix(&root)
            .map_err(|_| SlopError::TierRecallTargetOutsideRepo(target.clone()))?
            .to_string_lossy()
            .replace('\\', "/");
        targets.push(rel);
    }
    let rows = score_tier_recall(&tower, &targets);
    let tier_one_or_better = rows
        .iter()
        .filter(|row| row.tier.is_some_and(|tier| tier <= Tier::One))
        .count();
    println!("path\ttier");
    for row in &rows {
        println!("{}\t{}", row.rel, tier_label(row.tier));
    }
    eprintln!(
        "tier recall: {tier_one_or_better}/{} targets reached tier 0/1",
        rows.len()
    );
    Ok(())
}

pub fn score_tier_recall(tower: &TowerGraph, targets: &[String]) -> Vec<TierRecall> {
    let tiers: BTreeMap<&str, Tier> = tower
        .members
        .iter()
        .map(|member| (member.rel.as_str(), member.tier))
        .collect();
    let mut rows: Vec<TierRecall> = targets
        .iter()
        .map(|rel| TierRecall {
            rel: rel.clone(),
            tier: tiers.get(rel.as_str()).copied(),
        })
        .collect();
    rows.sort_by(|left, right| left.rel.cmp(&right.rel));
    rows
}

fn tier_label(tier: Option<Tier>) -> &'static str {
    match tier {
        Some(Tier::Zero) => "tier-0",
        Some(Tier::One) => "tier-1",
        Some(Tier::Two) => "tier-2",
        Some(Tier::Three) => "tier-3",
        None => "absent",
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_recall_reports_tiers_and_absent_targets_in_path_order() {
        let tower = TowerGraph {
            schema: 1,
            repo_id: "repo".to_string(),
            seed_digest: "seed".to_string(),
            seeds: vec!["src/rule.rs".to_string()],
            project_graph_fingerprint: "graph".to_string(),
            ranking_fingerprint: "ranking".to_string(),
            generated_at_unix: 0,
            members: vec![
                crate::graphstore::TowerMember {
                    rel: "src/rule.rs".to_string(),
                    tier: Tier::Zero,
                    score: 1.0,
                    via: Vec::new(),
                },
                crate::graphstore::TowerMember {
                    rel: "src/dispatcher.rs".to_string(),
                    tier: Tier::One,
                    score: 0.1,
                    via: vec!["src/rule.rs".to_string()],
                },
            ],
            cut_scores: [0.0; 3],
        };
        let rows = score_tier_recall(
            &tower,
            &[
                "src/missing.rs".to_string(),
                "src/dispatcher.rs".to_string(),
            ],
        );
        assert_eq!(rows[0].rel, "src/dispatcher.rs");
        assert_eq!(rows[0].tier, Some(Tier::One));
        assert_eq!(rows[1].rel, "src/missing.rs");
        assert_eq!(rows[1].tier, None);
    }
}
