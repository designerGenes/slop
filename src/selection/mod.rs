pub mod budget;
pub mod index;
pub mod query;
pub mod traverse;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::SlopError;
use crate::models::CliArgs;

#[derive(Debug, Clone)]
pub enum SelectionReason {
    Seed,
    Neighbor { hops: usize },
    Symbol,
    Match,
    Task,
}

#[derive(Debug, Clone)]
pub struct ScoredPath {
    pub path: PathBuf,
    pub rel_path: String,
    pub score: f32,
    pub reason: SelectionReason,
}

#[derive(Debug, Clone)]
pub struct Selectors {
    pub matches: Vec<String>,
    pub seeds: Vec<PathBuf>,
    pub hops: usize,
    pub symbols: Vec<String>,
    pub task: Option<String>,
}

#[derive(Debug)]
pub struct Selection {
    pub selected: Vec<ScoredPath>,
    pub dropped: Vec<ScoredPath>,
}

pub fn selection_mode(args: &CliArgs) -> bool {
    !args.matches.is_empty()
        || !args.seeds.is_empty()
        || !args.symbols.is_empty()
        || args.task.is_some()
}

pub fn build_selectors(args: &CliArgs, config: &Config) -> Result<Selectors, SlopError> {
    if args.task.is_some() && !config.allow_fuzzy_task {
        return Err(SlopError::FuzzyTaskDisabled);
    }

    Ok(Selectors {
        matches: args.matches.clone(),
        seeds: args.seeds.clone(),
        hops: args.hops.unwrap_or(config.selection_default_hops),
        symbols: args.symbols.clone(),
        task: args.task.clone(),
    })
}

pub fn select_files(
    selectors: &Selectors,
    corpus_root: &Path,
    map_reserve_bytes: usize,
    config: &Config,
    force_reindex: bool,
) -> Result<Selection, SlopError> {
    let top_k = config.top_k;

    let mut candidates: Vec<ScoredPath> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // Tier 0: seeds (always included, score = +inf)
    for seed in &selectors.seeds {
        let abs = if seed.is_absolute() {
            seed.clone()
        } else {
            corpus_root.join(seed)
        };
        let rel = abs
            .strip_prefix(corpus_root)
            .unwrap_or(&abs)
            .to_string_lossy()
            .to_string();

        if seen.insert(rel.clone()) {
            candidates.push(ScoredPath {
                path: abs,
                rel_path: rel,
                score: f32::INFINITY,
                reason: SelectionReason::Seed,
            });
        }
    }

    // Tier 1: BFS neighbors of seeds
    if !selectors.seeds.is_empty() && selectors.hops > 0 {
        let adjacency = traverse::build_adjacency(corpus_root);
        let neighbors = traverse::bfs_neighbors(&adjacency, &selectors.seeds, corpus_root, selectors.hops);

        for (rel, depth) in neighbors {
            if seen.insert(rel.clone()) {
                let abs = traverse::rel_to_abs(corpus_root, &rel);
                candidates.push(ScoredPath {
                    path: abs,
                    rel_path: rel,
                    score: 1.0 / (1 + depth) as f32,
                    reason: SelectionReason::Neighbor { hops: selectors.hops },
                });
            }
        }
    }

    // Tier 2: symbol resolution
    for symbol in &selectors.symbols {
        let symbol_files = traverse::resolve_symbol(corpus_root, symbol);
        if symbol_files.is_empty() {
            eprintln!("warning: symbol '{}' not found; skipping", symbol);
            continue;
        }
        for rel in symbol_files {
            if seen.insert(rel.clone()) {
                let abs = traverse::rel_to_abs(corpus_root, &rel);
                candidates.push(ScoredPath {
                    path: abs,
                    rel_path: rel,
                    score: 1.0,
                    reason: SelectionReason::Symbol,
                });
            }
        }
    }

    // Tier 3: BM25 match query
    let need_index = !selectors.matches.is_empty() || selectors.task.is_some();
    if need_index {
        let (index, reader, fields) =
            index::ensure_index(corpus_root, config, force_reindex)?;

        if !selectors.matches.is_empty() {
            let match_results = query::run_match_query(
                &index, &reader, &fields, &selectors.matches, top_k,
            )?;
            for sp in match_results {
                if seen.insert(sp.rel_path.clone()) {
                    candidates.push(sp);
                }
            }
        }

        // Tier 4: fuzzy task query
        if let Some(task) = &selectors.task {
            let task_results = query::run_task_query(
                &index, &reader, &fields, task, top_k,
            )?;
            for sp in task_results {
                if seen.insert(sp.rel_path.clone()) {
                    candidates.push(sp);
                }
            }
        }
    }

    if candidates.is_empty() {
        eprintln!("warning: no files matched selection criteria; falling back to all files");
    }

    // Budget fill
    let (selected, dropped) = budget::fill_budget(
        &candidates,
        config.max_slop_bytes,
        map_reserve_bytes,
        top_k,
    )?;

    Ok(Selection { selected, dropped })
}

pub fn build_provenance_block(
    selection: &Selection,
    selectors: &Selectors,
    max_bytes: usize,
) -> crate::models::SoupMetaBlock {
    let mut lines = Vec::new();

    let match_str = if selectors.matches.is_empty() {
        "none".to_string()
    } else {
        selectors.matches.join(",")
    };
    let seed_str = if selectors.seeds.is_empty() {
        "none".to_string()
    } else {
        selectors
            .seeds
            .iter()
            .map(|s| s.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(",")
    };
    let symbol_str = if selectors.symbols.is_empty() {
        "none".to_string()
    } else {
        selectors.symbols.join(",")
    };
    let task_str = selectors.task.as_deref().unwrap_or("none");

    lines.push(format!(
        "selectors: match=[{}] seed=[{}] hops={} symbol=[{}] task={}",
        match_str, seed_str, selectors.hops, symbol_str, task_str
    ));

    for sp in &selection.selected {
        let reason_str = match &sp.reason {
            SelectionReason::Seed => "seed".to_string(),
            SelectionReason::Neighbor { hops } => format!("neighbor_{}h", hops),
            SelectionReason::Symbol => "symbol".to_string(),
            SelectionReason::Match => "match".to_string(),
            SelectionReason::Task => "task".to_string(),
        };
        lines.push(format!("{}\t{}\t{:.4}", sp.rel_path, reason_str, sp.score));
    }

    lines.push(format!("dropped: {} files cut for budget", selection.dropped.len()));

    let mut content = lines.join("\n");
    if content.len() > max_bytes.saturating_sub(100) {
        content = content.chars().take(max_bytes.saturating_sub(120)).collect();
        content.push_str("\n... (truncated)");
    }

    let content_lines: Vec<String> = content.split('\n').map(ToString::to_string).collect();
    let line_count = content_lines.len();

    crate::models::SoupMetaBlock {
        label: "selection".to_string(),
        kind: "selection".to_string(),
        format: "text".to_string(),
        line_count,
        readonly: true,
        content_lines,
    }
}
