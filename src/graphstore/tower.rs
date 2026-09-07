use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::Config;

use super::model::ProjectGraph;
use super::store;

pub const TOWER_GRAPH_SCHEMA: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tier {
    Zero,
    One,
    Two,
    Three,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TowerMember {
    pub rel: String,
    pub tier: Tier,
    pub score: f64,
    pub via: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TowerGraph {
    pub schema: u32,
    pub repo_id: String,
    pub seed_digest: String,
    pub seeds: Vec<String>,
    pub project_graph_fingerprint: String,
    /// Settings that influence ranking but are not part of the project graph.
    /// Prevents a cached tower from surviving a ranking-tuning change.
    pub ranking_fingerprint: String,
    pub generated_at_unix: u64,
    pub members: Vec<TowerMember>,
    pub cut_scores: [f64; 3],
}

pub fn build_tower_graph(project: &ProjectGraph, seeds: &[String], config: &Config) -> TowerGraph {
    let seed_set: BTreeSet<&str> = seeds.iter().map(String::as_str).collect();
    let indices: BTreeMap<&str, usize> = project
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| (file.rel.as_str(), index))
        .collect();
    #[derive(Default)]
    struct EdgeWeights {
        symbol: f64,
        cochange: f64,
    }

    let mut raw = BTreeMap::<(usize, usize), EdgeWeights>::new();
    for edge in &project.symbol_edges {
        if let (Some(&a), Some(&b)) = (
            indices.get(edge.from.as_str()),
            indices.get(edge.to.as_str()),
        ) {
            if a != b {
                raw.entry(if a < b { (a, b) } else { (b, a) })
                    .or_default()
                    .symbol += edge.weight;
            }
        }
    }
    for edge in &project.cochange_edges {
        if let (Some(&a), Some(&b)) = (indices.get(edge.a.as_str()), indices.get(edge.b.as_str())) {
            if a != b {
                raw.entry(if a < b { (a, b) } else { (b, a) })
                    .or_default()
                    .cochange += config.graph_cochange_weight * edge.weight;
            }
        }
    }

    let mut adjacency = vec![Vec::<(usize, f64)>::new(); project.files.len()];
    for ((a, b), weights) in raw {
        let same_community = project.files[a].community.is_some()
            && project.files[a].community == project.files[b].community;
        let degree_hint = (project.files[a].afferent + project.files[a].efferent)
            .max(project.files[b].afferent + project.files[b].efferent)
            .max(1) as f64;
        // Default 0.5 preserves the prior shared square-root damping exactly,
        // while allowing focused tuning of symbol-only dispatcher edges.
        let symbol_damped = weights.symbol
            / degree_hint.powf(config.tower_symbol_hub_damping_exponent.clamp(0.0, 1.0));
        let non_symbol = weights.cochange
            + if same_community {
                config.tower_community_bonus
            } else {
                0.0
            };
        let damped = symbol_damped + non_symbol / degree_hint.sqrt();
        if damped <= 0.0 {
            continue;
        }
        adjacency[a].push((b, damped));
        adjacency[b].push((a, damped));
    }
    for neighbors in &mut adjacency {
        neighbors.sort_by_key(|(index, _)| *index);
    }
    let degrees: Vec<f64> = adjacency
        .iter()
        .map(|neighbors| neighbors.iter().map(|(_, weight)| weight).sum())
        .collect();
    let n = project.files.len();
    let mut personalization = vec![0.0; n];
    if !seed_set.is_empty() {
        for (index, file) in project.files.iter().enumerate() {
            if seed_set.contains(file.rel.as_str()) {
                personalization[index] = 1.0 / seed_set.len() as f64;
            }
        }
    }
    let mut ranks = personalization.clone();
    for _ in 0..config.tower_rwr_iterations {
        let mut next: Vec<f64> = personalization
            .iter()
            .map(|score| config.tower_restart_alpha * score)
            .collect();
        for source in 0..n {
            if degrees[source] == 0.0 {
                continue;
            }
            for &(target, weight) in &adjacency[source] {
                next[target] +=
                    (1.0 - config.tower_restart_alpha) * ranks[source] * weight / degrees[source];
            }
        }
        let total: f64 = next.iter().sum();
        if total > 0.0 {
            for value in &mut next {
                *value /= total;
            }
        }
        let delta: f64 = next.iter().zip(&ranks).map(|(a, b)| (a - b).abs()).sum();
        ranks = next;
        if delta < config.tower_rwr_epsilon {
            break;
        }
    }

    let mut non_seeds: Vec<usize> = (0..n)
        .filter(|index| !seed_set.contains(project.files[*index].rel.as_str()))
        .collect();
    non_seeds.sort_by(|a, b| {
        ranks[*b]
            .partial_cmp(&ranks[*a])
            .unwrap_or(Ordering::Equal)
            .then_with(|| project.files[*a].rel.cmp(&project.files[*b].rel))
    });
    let total_mass: f64 = non_seeds.iter().map(|index| ranks[*index]).sum();
    let mut tier_by_index = BTreeMap::new();
    let mut running = 0.0;
    let mut cuts = [0.0; 3];
    for index in non_seeds {
        if ranks[index] < config.tower_tier3_min_score {
            continue;
        }
        running += ranks[index];
        let fraction = if total_mass > 0.0 {
            running / total_mass
        } else {
            1.0
        };
        // A single reachable file carries all non-seed mass. Keep that direct
        // neighbor useful instead of demoting it merely because it crosses all
        // cumulative boundaries at once.
        let tier = if running == ranks[index] || fraction <= config.tower_tier1_mass_fraction {
            Tier::One
        } else if fraction <= config.tower_tier2_mass_fraction {
            Tier::Two
        } else {
            Tier::Three
        };
        match tier {
            Tier::One => cuts[0] = ranks[index],
            Tier::Two => cuts[1] = ranks[index],
            Tier::Three => cuts[2] = ranks[index],
            Tier::Zero => {}
        }
        tier_by_index.insert(index, tier);
    }
    promote_structural_hubs(project, &seed_set, &mut tier_by_index, config);
    let mut members = Vec::new();
    for (index, file) in project.files.iter().enumerate() {
        let tier = if seed_set.contains(file.rel.as_str()) {
            Some(Tier::Zero)
        } else {
            tier_by_index.get(&index).copied()
        };
        let Some(tier) = tier else {
            continue;
        };
        let mut via: Vec<(usize, f64)> = adjacency[index]
            .iter()
            .copied()
            .filter(|(neighbor, _)| {
                let neighbor_tier = if seed_set.contains(project.files[*neighbor].rel.as_str()) {
                    Some(Tier::Zero)
                } else {
                    tier_by_index.get(neighbor).copied()
                };
                neighbor_tier.is_some_and(|candidate| candidate < tier)
            })
            .collect();
        via.sort_by(|(left_index, left_weight), (right_index, right_weight)| {
            right_weight
                .partial_cmp(left_weight)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    project.files[*left_index]
                        .rel
                        .cmp(&project.files[*right_index].rel)
                })
        });
        members.push(TowerMember {
            rel: file.rel.clone(),
            tier,
            score: ranks[index],
            via: via
                .into_iter()
                .take(3)
                .map(|(neighbor, _)| project.files[neighbor].rel.clone())
                .collect(),
        });
    }
    members.sort_by(|left, right| {
        left.tier
            .cmp(&right.tier)
            .then_with(|| {
                right
                    .score
                    .partial_cmp(&left.score)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| left.rel.cmp(&right.rel))
    });
    TowerGraph {
        schema: TOWER_GRAPH_SCHEMA,
        repo_id: project.repo_id.clone(),
        seed_digest: store::seed_digest(seeds),
        seeds: seeds.to_vec(),
        project_graph_fingerprint: project.fingerprint(),
        ranking_fingerprint: ranking_fingerprint(config),
        generated_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        members,
        cut_scores: cuts,
    }
}

/// Stable identity of every configuration setting that can alter tower tiers.
pub fn ranking_fingerprint(config: &Config) -> String {
    let settings = format!(
        "{:.17}|{}|{:.17}|{:.17}|{:.17}|{:.17}|{:.17}|{:.17}|{:.17}|{:.17}|{}",
        config.graph_cochange_weight,
        config.tower_rwr_iterations,
        config.tower_restart_alpha,
        config.tower_rwr_epsilon,
        config.tower_community_bonus,
        config.tower_symbol_hub_damping_exponent,
        config.tower_tier1_mass_fraction,
        config.tower_tier2_mass_fraction,
        config.tower_tier3_min_score,
        config.tower_structural_hub_min_fraction,
        config.tower_structural_hub_min_links,
    );
    blake3::hash(settings.as_bytes()).to_hex().to_string()
}

/// Recover files whose relevance is broad rather than pairwise. Generic hubs
/// remain damped in RWR, but a dispatcher or registry that touches a sizeable
/// share of the seed's own community is promoted to tier 1 after ranking.
fn promote_structural_hubs(
    project: &ProjectGraph,
    seed_set: &BTreeSet<&str>,
    tier_by_index: &mut BTreeMap<usize, Tier>,
    config: &Config,
) {
    let mut seed_communities = BTreeMap::<usize, usize>::new();
    for seed in seed_set {
        if let Some(community) = project.file(seed).and_then(|file| file.community) {
            *seed_communities.entry(community).or_default() += 1;
        }
    }
    if let Some((community, seed_count)) =
        seed_communities
            .into_iter()
            .max_by(|(left_id, left_count), (right_id, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| right_id.cmp(left_id))
            })
    {
        // A split seed set has no single module to use as a structural reference.
        if seed_count * 2 > seed_set.len()
            && let Some(community_size) = project
                .community(community)
                .map(|entry| entry.members.len())
            && community_size > 0
        {
            let couplings: BTreeMap<&str, usize> = project
                .community_couplings
                .iter()
                .filter(|entry| entry.community == community)
                .map(|entry| (entry.candidate.as_str(), entry.distinct_members))
                .collect();
            promote_coupled_files(
                project,
                seed_set,
                tier_by_index,
                &couplings,
                community_size,
                None,
                config,
            );
        }
    }

    let directories: BTreeSet<&str> = seed_set.iter().map(|seed| parent_directory(seed)).collect();
    if directories.len() != 1 {
        return;
    }
    let directory = directories.iter().next().copied().expect("one directory");
    let directory_size = project
        .files
        .iter()
        .filter(|file| parent_directory(&file.rel) == directory)
        .count();
    if directory_size == 0 {
        return;
    }
    let couplings: BTreeMap<&str, usize> = project
        .directory_couplings
        .iter()
        .filter(|entry| entry.directory == directory)
        .map(|entry| (entry.candidate.as_str(), entry.distinct_members))
        .collect();
    promote_coupled_files(
        project,
        seed_set,
        tier_by_index,
        &couplings,
        directory_size,
        Some(directory),
        config,
    );
    // A Rust module's explicit entry point is part of the seed's local
    // structure, not an arbitrary same-directory neighbour. Keep it available
    // even though directory aggregation deliberately only promotes outward.
    let module_entry = if directory == "." {
        "mod.rs".to_string()
    } else {
        format!("{directory}/mod.rs")
    };
    if let Some(index) = project
        .files
        .iter()
        .position(|file| file.rel == module_entry)
        && !seed_set.contains(project.files[index].rel.as_str())
    {
        tier_by_index.insert(index, Tier::One);
    }
}

fn promote_coupled_files(
    project: &ProjectGraph,
    seed_set: &BTreeSet<&str>,
    tier_by_index: &mut BTreeMap<usize, Tier>,
    couplings: &BTreeMap<&str, usize>,
    scope_size: usize,
    outside_directory: Option<&str>,
    config: &Config,
) {
    let fraction = config.tower_structural_hub_min_fraction.clamp(0.0, 1.0);
    for (index, file) in project.files.iter().enumerate() {
        if seed_set.contains(file.rel.as_str()) || tier_by_index.get(&index) == Some(&Tier::One) {
            continue;
        }
        // Aggregate coupling can refine a weak graph path, but it must not
        // manufacture relevance for a graph-disconnected file from a broad
        // one-off commit alone.
        if !tier_by_index.contains_key(&index) {
            continue;
        }
        if outside_directory.is_some_and(|directory| parent_directory(&file.rel) == directory) {
            continue;
        }
        let links = couplings
            .get(file.rel.as_str())
            .copied()
            .unwrap_or_default();
        if links >= config.tower_structural_hub_min_links
            && links as f64 / scope_size as f64 >= fraction
        {
            tier_by_index.insert(index, Tier::One);
        }
    }
}

fn parent_directory(rel: &str) -> &str {
    rel.rsplit_once('/').map_or(".", |(directory, _)| directory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphstore::model::{
        Community, CommunityCoupling, FileEntry, GraphStats, PROJECT_GRAPH_SCHEMA, Structure,
        SymbolEdge,
    };

    fn graph(edges: &[(&str, &str)]) -> ProjectGraph {
        let files = ["a.rs", "b.rs", "c.rs", "d.rs", "hub.rs"]
            .into_iter()
            .map(|rel| FileEntry {
                rel: rel.to_string(),
                blake3: "0".repeat(64),
                size: 1,
                parsed: true,
                tags: Vec::new(),
                rank: 0.0,
                afferent: 1,
                efferent: 1,
                instability: 0.5,
                def_count: 0,
                community: None,
            })
            .collect();
        ProjectGraph {
            schema: PROJECT_GRAPH_SCHEMA,
            repo_root: "/repo".to_string(),
            repo_id: "repo".to_string(),
            generated_at_unix: 0,
            generator_version: "test".to_string(),
            files,
            symbol_edges: edges
                .iter()
                .map(|(from, to)| SymbolEdge {
                    from: (*from).to_string(),
                    to: (*to).to_string(),
                    weight: 1.0,
                    idents: Vec::new(),
                })
                .collect(),
            cochange_edges: Vec::new(),
            communities: Vec::new(),
            community_couplings: Vec::new(),
            directory_couplings: Vec::new(),
            structure: Structure::default(),
            stats: GraphStats::default(),
        }
    }

    #[test]
    fn an_isolated_seed_is_the_entire_tower() {
        let tower = build_tower_graph(&graph(&[]), &["a.rs".to_string()], &Config::default());
        assert_eq!(tower.members.len(), 1);
        assert_eq!(tower.members[0].rel, "a.rs");
        assert_eq!(tower.members[0].tier, Tier::Zero);
    }

    #[test]
    fn disconnected_files_do_not_reach_tier_one() {
        let tower = build_tower_graph(
            &graph(&[("a.rs", "b.rs"), ("c.rs", "d.rs")]),
            &["a.rs".to_string()],
            &Config::default(),
        );
        assert!(
            tower
                .members
                .iter()
                .any(|member| member.rel == "b.rs" && member.tier == Tier::One)
        );
        assert!(
            tower
                .members
                .iter()
                .all(|member| !matches!(member.rel.as_str(), "c.rs" | "d.rs")
                    || member.tier != Tier::One)
        );
    }

    #[test]
    fn tower_contents_are_deterministic() {
        let project = graph(&[("a.rs", "b.rs"), ("b.rs", "c.rs")]);
        let seeds = vec!["a.rs".to_string()];
        let first = build_tower_graph(&project, &seeds, &Config::default());
        let second = build_tower_graph(&project, &seeds, &Config::default());
        let mut first = serde_json::to_value(first).expect("serialize");
        let mut second = serde_json::to_value(second).expect("serialize");
        first.as_object_mut().unwrap().remove("generated_at_unix");
        second.as_object_mut().unwrap().remove("generated_at_unix");
        assert_eq!(first, second);
    }

    #[test]
    fn aggregate_community_coupling_promotes_an_under_ranked_structural_hub() {
        let mut project = graph(&[("a.rs", "b.rs"), ("b.rs", "c.rs"), ("b.rs", "hub.rs")]);
        project
            .symbol_edges
            .iter_mut()
            .find(|edge| edge.from == "a.rs" && edge.to == "b.rs")
            .expect("seed edge exists")
            .weight = 100.0;
        for file in &mut project.files {
            if matches!(file.rel.as_str(), "a.rs" | "b.rs" | "c.rs") {
                file.community = Some(0);
            }
        }
        project.communities = vec![Community {
            id: 0,
            label: "rules".to_string(),
            members: vec!["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()],
            internal_weight: 1.0,
            external_weight: 1.0,
        }];
        project.community_couplings = vec![CommunityCoupling {
            candidate: "hub.rs".to_string(),
            community: 0,
            distinct_members: 2,
        }];

        let mut disabled = Config::default();
        disabled.tower_structural_hub_min_fraction = 1.0;
        disabled.tower_tier1_mass_fraction = 0.1;
        let without_promotion = build_tower_graph(&project, &["a.rs".to_string()], &disabled);
        assert!(
            without_promotion
                .members
                .iter()
                .any(|member| member.rel == "hub.rs" && member.tier != Tier::One),
            "the hub must not already be tier 1 for this test to prove the override"
        );

        let mut enabled = disabled;
        enabled.tower_structural_hub_min_fraction = 0.5;
        let promoted = build_tower_graph(&project, &["a.rs".to_string()], &enabled);
        assert!(
            promoted
                .members
                .iter()
                .any(|member| member.rel == "hub.rs" && member.tier == Tier::One),
            "a hub linked to 2/3 seed-community members should be promoted"
        );
    }

    #[test]
    fn seed_directory_fallback_promotes_when_louvain_community_is_unavailable() {
        let mut project = graph(&[("a.rs", "b.rs"), ("b.rs", "c.rs"), ("b.rs", "hub.rs")]);
        project
            .symbol_edges
            .iter_mut()
            .find(|edge| edge.from == "a.rs" && edge.to == "b.rs")
            .expect("seed edge exists")
            .weight = 100.0;
        for file in &mut project.files {
            if file.rel == "hub.rs" {
                file.rel = "src/hub.rs".to_string();
            }
        }
        for edge in &mut project.symbol_edges {
            if edge.to == "hub.rs" {
                edge.to = "src/hub.rs".to_string();
            }
        }
        project.directory_couplings = vec![crate::graphstore::model::DirectoryCoupling {
            candidate: "src/hub.rs".to_string(),
            directory: ".".to_string(),
            distinct_members: 2,
        }];

        let mut config = Config::default();
        config.tower_tier1_mass_fraction = 0.1;
        config.tower_structural_hub_min_fraction = 0.35;
        let tower = build_tower_graph(&project, &["a.rs".to_string()], &config);
        assert!(
            tower
                .members
                .iter()
                .any(|member| member.rel == "src/hub.rs" && member.tier == Tier::One),
            "the directory fallback should recover a hub coupled to 2/5 peers"
        );
    }

    #[test]
    fn aggregate_coupling_cannot_promote_a_graph_disconnected_file() {
        let mut project = graph(&[("a.rs", "b.rs")]);
        for file in &mut project.files {
            if matches!(file.rel.as_str(), "a.rs" | "b.rs") {
                file.community = Some(0);
            }
        }
        project.communities = vec![Community {
            id: 0,
            label: "rules".to_string(),
            members: vec!["a.rs".to_string(), "b.rs".to_string()],
            internal_weight: 1.0,
            external_weight: 0.0,
        }];
        project.community_couplings = vec![CommunityCoupling {
            candidate: "d.rs".to_string(),
            community: 0,
            distinct_members: 2,
        }];

        let tower = build_tower_graph(&project, &["a.rs".to_string()], &Config::default());
        assert!(
            tower.members.iter().all(|member| member.rel != "d.rs"),
            "raw aggregate evidence must not pull a disconnected file into scope"
        );
    }

    #[test]
    fn ranking_fingerprint_changes_with_tower_settings() {
        let original = Config::default();
        let mut adjusted = original.clone();
        adjusted.tower_symbol_hub_damping_exponent = 0.35;
        assert_ne!(
            ranking_fingerprint(&original),
            ranking_fingerprint(&adjusted)
        );
    }
}
