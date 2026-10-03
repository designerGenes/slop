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
    /// 0-based index in `tower.members` — the ordering a manifest page takes
    /// its files from before task-relevance promotion. `None` when the file is
    /// absent from the tower.
    pub rank: Option<usize>,
    /// Whether the file is in the page a real `--page-open --manifest` would
    /// build for this task: score order plus any reserved task-relevance
    /// slots. A tier-1 file at rank 278 of a 32-file window is NOT delivered;
    /// the tier alone must never read as success.
    pub in_page: bool,
    /// The qualifier-probe selection score for this file, when a task with a
    /// qualifier was supplied. Explains why a file is or is not promoted.
    pub selection_score: Option<usize>,
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
    let cap = config.page_manifest_max_files.max(1);
    // `in_page` must reflect the real page selection, including the reserved
    // task-relevance slots, or it repeats the tier-only illusion this column
    // exists to kill.
    let selection = crate::page::select_manifest_page(
        &tower,
        config,
        args.task.as_deref(),
        &root,
        args.verbose || config.verbose_output,
    );
    let window: BTreeSet<usize> = selection.indices.iter().copied().collect();
    let mut rows = score_tier_recall(&tower, &targets, &window);
    let history = tower.seeds.first().and_then(|seed| {
        crate::history_select::sibling_additions(
            &root,
            seed,
            config.page_history_commit_window,
            None,
        )
    });
    if let Some(history) = &history {
        // Show the history tally for every target, even one already inside the
        // window: the tally explains the signal, while `signal` below names
        // which path actually admitted the promoted files.
        for row in &mut rows {
            if let Some(rank) = row.rank {
                row.selection_score = history.tally.get(&tower.members[rank].rel).copied();
            }
        }
    } else if selection.signal == "qualifier" {
        let qualifier = crate::anchor::qualifier_tokens(args.task.as_deref().unwrap_or(""));
        if !qualifier.is_empty() {
            for row in &mut rows {
                if let Some(rank) = row.rank {
                    let rel = &tower.members[rank].rel;
                    if let Ok(text) = fs::read_to_string(root.join(rel)) {
                        row.selection_score =
                            Some(crate::anchor::selection_score(&text, rel, &qualifier));
                    }
                }
            }
        }
    }
    let tier_one_or_better = rows
        .iter()
        .filter(|row| row.tier.is_some_and(|tier| tier <= Tier::One))
        .count();
    println!("# page window: {cap} files (page_manifest_max_files)");
    if config.page_task_relevance_promotion && args.task.is_some() {
        println!(
            "# task-relevance promotion: on ({} reserved slots; {} promoted into the page)",
            config.page_task_relevance_reserved_slots,
            selection.promoted.len()
        );
        if selection.signal == "history" {
            println!(
                "# selection signal: history ({} prior sibling-additions)",
                selection.prior_commits.unwrap_or(0)
            );
        } else {
            println!("# selection signal: {}", selection.signal);
        }
        if let Some(history) = &history {
            println!(
                "# history tally available: {} prior sibling-additions",
                history.prior_commits
            );
        }
    }
    println!("path\ttier\trank\tin_page\tscore");
    for row in &rows {
        let rank = row.rank.map_or(String::new(), |rank| rank.to_string());
        let score = row
            .selection_score
            .map_or(String::new(), |score| score.to_string());
        println!(
            "{}\t{}\t{}\t{}\t{}",
            row.rel,
            tier_label(row.tier),
            rank,
            row.in_page,
            score
        );
    }
    eprintln!(
        "tier recall: {tier_one_or_better}/{} targets reached tier 0/1",
        rows.len()
    );
    Ok(())
}

pub fn score_tier_recall(
    tower: &TowerGraph,
    targets: &[String],
    page_window: &BTreeSet<usize>,
) -> Vec<TierRecall> {
    let members: BTreeMap<&str, (usize, Tier)> = tower
        .members
        .iter()
        .enumerate()
        .map(|(index, member)| (member.rel.as_str(), (index, member.tier)))
        .collect();
    let mut rows: Vec<TierRecall> = targets
        .iter()
        .map(|rel| match members.get(rel.as_str()) {
            Some((rank, tier)) => TierRecall {
                rel: rel.clone(),
                tier: Some(*tier),
                rank: Some(*rank),
                in_page: page_window.contains(rank),
                selection_score: None,
            },
            None => TierRecall {
                rel: rel.clone(),
                tier: None,
                rank: None,
                in_page: false,
                selection_score: None,
            },
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
    fn tier_recall_reports_tiers_ranks_and_page_inclusion_in_path_order() {
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
                crate::graphstore::TowerMember {
                    rel: "src/out_of_window.rs".to_string(),
                    tier: Tier::One,
                    score: 0.01,
                    via: Vec::new(),
                },
            ],
            cut_scores: [0.0; 3],
        };
        let rows = score_tier_recall(
            &tower,
            &[
                "src/missing.rs".to_string(),
                "src/out_of_window.rs".to_string(),
                "src/dispatcher.rs".to_string(),
            ],
            &[0, 1].into_iter().collect(),
        );
        assert_eq!(rows[0].rel, "src/dispatcher.rs");
        assert_eq!(rows[0].tier, Some(Tier::One));
        assert_eq!(rows[0].rank, Some(1));
        assert!(rows[0].in_page);
        assert_eq!(rows[1].rel, "src/missing.rs");
        assert_eq!(rows[1].tier, None);
        assert_eq!(rows[1].rank, None);
        assert!(!rows[1].in_page);
        assert_eq!(rows[2].rel, "src/out_of_window.rs");
        assert_eq!(rows[2].rank, Some(2));
        assert!(
            !rows[2].in_page,
            "rank 2 of a 2-file window is not delivered"
        );
    }
}
