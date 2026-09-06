//! Building and refreshing a project graph.
//!
//! The expensive step is tree-sitter parsing, and it is the only step this
//! module tries to avoid. Ranking, community detection and structure analysis
//! all run over the complete tag set on every build, even an incremental one.
//! That is deliberate: an incrementally-maintained PageRank would drift away
//! from what a cold build produces, and a cache that disagrees with a rebuild is
//! worse than no cache. Parsing is skipped per file and only when the file's
//! blake3 is byte-identical to the stored one.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::SlopError;
use crate::repomap::graph::{self as repograph};
use crate::repomap::manifest;
use crate::repomap::tags;

use super::cochange::{self, CoChangeOptions};
use super::community::{UndirectedGraph, louvain, modularity};
use super::model::{
    BuildReport, CoChangeEdge, Community, FileEntry, GraphStats, PROJECT_GRAPH_SCHEMA,
    ProjectGraph, StoredTag, Structure, SymbolEdge,
};
use super::store;

pub struct BuildOptions {
    pub max_files: usize,
    pub community_resolution: f64,
    /// Multiplier applied to co-change weights before they join the symbol
    /// edges for clustering. Below 1.0 because a proven symbol reference is
    /// stronger evidence than a shared commit.
    pub cochange_weight: f64,
    pub cochange: CoChangeOptions,
    /// Ignore any stored graph and parse everything.
    pub force_rebuild: bool,
}

impl BuildOptions {
    pub fn from_config(config: &Config, force_rebuild: bool) -> Self {
        Self {
            max_files: config.graph_max_files,
            community_resolution: config.graph_community_resolution,
            cochange_weight: config.graph_cochange_weight,
            cochange: CoChangeOptions {
                max_commits: config.graph_cochange_commits,
                max_files_per_commit: config.graph_cochange_max_files_per_commit,
                min_commits: config.graph_cochange_min_commits,
                max_edges: config.graph_cochange_max_edges,
            },
            force_rebuild,
        }
    }
}

/// Build a graph for `repo_root`, reusing `previous` where the bytes agree.
pub fn build_project_graph(
    repo_root: &Path,
    previous: Option<&ProjectGraph>,
    options: &BuildOptions,
) -> Result<(ProjectGraph, BuildReport), SlopError> {
    let started = Instant::now();
    let previous = if options.force_rebuild {
        None
    } else {
        previous
    };

    let manifest = manifest::collect_repo_files(repo_root, options.max_files);
    let cached: BTreeMap<&str, &FileEntry> =
        previous.map(|graph| graph.by_rel()).unwrap_or_default();

    let mut report = BuildReport {
        cold: previous.is_none(),
        ..BuildReport::default()
    };

    let mut entries: Vec<FileEntry> = Vec::with_capacity(manifest.files.len());
    let mut present: BTreeSet<String> = BTreeSet::new();

    for rel in &manifest.files {
        let absolute = repo_root.join(rel);
        let Ok(bytes) = fs::read(&absolute) else {
            // Unreadable in the split second since the walk: a build artifact
            // mid-delete, a permission quirk. Not worth failing a whole graph.
            continue;
        };
        let digest = blake3::hash(&bytes).to_hex().to_string();
        present.insert(rel.clone());

        let parseable = manifest::is_parseable(rel);
        let reusable = cached
            .get(rel.as_str())
            .filter(|entry| entry.blake3 == digest);

        // Only parseable files are counted in the reuse figures. Counting the
        // PNGs and lockfiles would inflate both sides and hide whether the
        // cache is actually saving any parsing.
        let (stored_tags, parsed) = match reusable {
            Some(entry) => {
                if parseable {
                    report.reused += 1;
                }
                (entry.tags.clone(), entry.parsed)
            }
            None if parseable => {
                report.reparsed += 1;
                let extracted = tags::extract_tags(&absolute.to_string_lossy(), rel);
                let parsed = !extracted.is_empty();
                (extracted.iter().map(StoredTag::from_tag).collect(), parsed)
            }
            None => (Vec::new(), false),
        };

        entries.push(FileEntry {
            rel: rel.clone(),
            blake3: digest,
            size: bytes.len() as u64,
            parsed,
            tags: stored_tags,
            rank: 0.0,
            afferent: 0,
            efferent: 0,
            instability: 1.0,
            def_count: 0,
            community: None,
        });
    }

    report.removed = cached.keys().filter(|rel| !present.contains(**rel)).count();

    entries.sort_by(|left, right| left.rel.cmp(&right.rel));

    // Rank over the full tag set every time. See the module comment.
    let all_tags: Vec<tags::Tag> = entries
        .iter()
        .flat_map(|entry| entry.tags.iter().map(|tag| tag.to_tag(&entry.rel)))
        .collect();

    let analysis = repograph::analyze(&all_tags, &HashSet::new(), &HashSet::new(), &HashSet::new());

    let metrics: BTreeMap<&str, &repograph::FileMetrics> = analysis
        .metrics
        .iter()
        .map(|metric| (metric.rel_fname.as_str(), metric))
        .collect();

    for entry in entries.iter_mut() {
        if let Some(metric) = metrics.get(entry.rel.as_str()) {
            entry.rank = metric.rank;
            entry.afferent = metric.afferent;
            entry.efferent = metric.efferent;
            entry.instability = metric.instability;
            entry.def_count = metric.def_count;
        }
    }

    let symbol_edges: Vec<SymbolEdge> = analysis
        .edges
        .iter()
        .map(|edge| SymbolEdge {
            from: edge.from.clone(),
            to: edge.to.clone(),
            weight: edge.weight,
            idents: edge.idents.clone(),
        })
        .collect();

    let tracked: BTreeSet<String> = entries.iter().map(|entry| entry.rel.clone()).collect();
    let cochange_result = cochange::mine(repo_root, &tracked, &options.cochange);
    report.commits_scanned = cochange_result.commits_scanned;

    let mut modularity_score = 0.0;
    let communities = detect_communities(
        &mut entries,
        &symbol_edges,
        &cochange_result.edges,
        options,
        &mut modularity_score,
    );

    let generated_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);

    let stats = GraphStats {
        file_count: entries.len(),
        parsed_count: entries.iter().filter(|entry| entry.parsed).count(),
        symbol_edge_count: symbol_edges.len(),
        cochange_edge_count: cochange_result.edges.len(),
        community_count: communities.len(),
        manifest_truncated: manifest.truncated,
        from_git: manifest.from_git,
        modularity: modularity_score,
    };

    let graph = ProjectGraph {
        schema: PROJECT_GRAPH_SCHEMA,
        repo_root: repo_root.to_string_lossy().to_string(),
        repo_id: store::repo_id(repo_root),
        generated_at_unix,
        generator_version: env!("CARGO_PKG_VERSION").to_string(),
        files: entries,
        symbol_edges,
        cochange_edges: cochange_result.edges,
        communities,
        structure: Structure {
            cycles: analysis.cycles.clone(),
            chokepoints: analysis.chokepoints.clone(),
            orphans: analysis.orphans.clone(),
        },
        stats,
    };

    report.elapsed_ms = started.elapsed().as_millis();
    Ok((graph, report))
}

/// Cluster the combined symbol + co-change graph and write the labels back onto
/// the file entries.
fn detect_communities(
    entries: &mut [FileEntry],
    symbol_edges: &[SymbolEdge],
    cochange_edges: &[CoChangeEdge],
    options: &BuildOptions,
    quality: &mut f64,
) -> Vec<Community> {
    // Owned keys, deliberately: the map is still in scope when `entries` is
    // borrowed mutably below, and borrowing the paths out of it would put the
    // two at odds for no gain worth the allocation saved.
    let index: BTreeMap<String, usize> = entries
        .iter()
        .enumerate()
        .map(|(position, entry)| (entry.rel.clone(), position))
        .collect();

    let mut edges: Vec<(usize, usize, f64)> = Vec::new();
    for edge in symbol_edges {
        if let (Some(&from), Some(&to)) = (index.get(&edge.from), index.get(&edge.to)) {
            edges.push((from, to, edge.weight));
        }
    }
    for edge in cochange_edges {
        if let (Some(&a), Some(&b)) = (index.get(&edge.a), index.get(&edge.b)) {
            edges.push((a, b, edge.weight * options.cochange_weight));
        }
    }

    let graph = UndirectedGraph::from_edges(entries.len(), &edges);
    let labels = louvain(&graph, options.community_resolution);
    *quality = modularity(&graph, &labels, options.community_resolution);

    let count = labels.iter().copied().max().map_or(0, |max| max + 1);
    let mut members: Vec<Vec<String>> = vec![Vec::new(); count];
    for (position, entry) in entries.iter_mut().enumerate() {
        let label = labels[position];
        entry.community = Some(label);
        members[label].push(entry.rel.clone());
    }

    let mut internal = vec![0.0; count];
    let mut external = vec![0.0; count];
    for &(from, to, weight) in &edges {
        let (left, right) = (labels[from], labels[to]);
        if left == right {
            internal[left] += weight;
        } else {
            external[left] += weight;
            external[right] += weight;
        }
    }

    (0..count)
        .map(|id| Community {
            id,
            label: community_label(&members[id]),
            members: members[id].clone(),
            internal_weight: internal[id],
            external_weight: external[id],
        })
        .collect()
}

/// Name a community after where its files live: the shared directory when there
/// is one, otherwise the directory holding most of them.
fn community_label(members: &[String]) -> String {
    if members.is_empty() {
        return "(empty)".to_string();
    }

    let directories: Vec<&str> = members
        .iter()
        .map(|rel| match rel.rfind('/') {
            Some(cut) => &rel[..cut],
            None => ".",
        })
        .collect();

    if let Some(shared) = common_prefix(&directories) {
        if !shared.is_empty() {
            return shared;
        }
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for directory in &directories {
        *counts.entry(directory).or_insert(0) += 1;
    }
    let (best, hits) = counts
        .iter()
        .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))
        .map(|(directory, count)| (*directory, *count))
        .unwrap_or((".", 0));

    if hits == members.len() {
        best.to_string()
    } else {
        format!("{best} +{}", members.len() - hits)
    }
}

/// Longest shared path prefix, on directory boundaries rather than characters,
/// so `src/repo` and `src/repomap` do not collapse into `src/repo`.
fn common_prefix(directories: &[&str]) -> Option<String> {
    let first = directories.first()?;
    let mut shared: Vec<&str> = first.split('/').collect();

    for directory in directories.iter().skip(1) {
        let candidate: Vec<&str> = directory.split('/').collect();
        let overlap = shared
            .iter()
            .zip(candidate.iter())
            .take_while(|(left, right)| left == right)
            .count();
        shared.truncate(overlap);
        if shared.is_empty() {
            return None;
        }
    }

    Some(shared.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shared_directory_names_the_community() {
        let members = vec![
            "src/repomap/graph.rs".to_string(),
            "src/repomap/tags.rs".to_string(),
        ];
        assert_eq!(community_label(&members), "src/repomap");
    }

    #[test]
    fn sibling_directories_fall_back_to_their_parent() {
        let members = vec![
            "src/repomap/graph.rs".to_string(),
            "src/selection/query.rs".to_string(),
        ];
        assert_eq!(community_label(&members), "src");
    }

    #[test]
    fn prefixes_break_on_path_separators_not_characters() {
        assert_eq!(
            common_prefix(&["src/repo", "src/repomap"]),
            Some("src".to_string())
        );
    }

    #[test]
    fn a_mixed_community_names_its_plurality_and_says_how_many_are_elsewhere() {
        let members = vec![
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "tests/c.rs".to_string(),
        ];
        assert_eq!(community_label(&members), "src +1");
    }

    #[test]
    fn root_level_files_do_not_produce_an_empty_label() {
        let members = vec!["build.rs".to_string(), "main.rs".to_string()];
        assert_eq!(community_label(&members), ".");
    }
}
