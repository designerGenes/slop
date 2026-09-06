use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::config::Config;

use super::model::ProjectGraph;
use super::store;

pub const TOWER_GRAPH_SCHEMA: u32 = 1;

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
    let mut raw = BTreeMap::<(usize, usize), f64>::new();
    for edge in &project.symbol_edges {
        if let (Some(&a), Some(&b)) = (
            indices.get(edge.from.as_str()),
            indices.get(edge.to.as_str()),
        ) {
            if a != b {
                *raw.entry(if a < b { (a, b) } else { (b, a) }).or_default() += edge.weight;
            }
        }
    }
    for edge in &project.cochange_edges {
        if let (Some(&a), Some(&b)) = (indices.get(edge.a.as_str()), indices.get(edge.b.as_str())) {
            if a != b {
                *raw.entry(if a < b { (a, b) } else { (b, a) }).or_default() +=
                    config.graph_cochange_weight * edge.weight;
            }
        }
    }

    let mut adjacency = vec![Vec::<(usize, f64)>::new(); project.files.len()];
    for ((a, b), weight) in raw {
        let same_community = project.files[a].community.is_some()
            && project.files[a].community == project.files[b].community;
        let weighted = weight
            + if same_community {
                config.tower_community_bonus
            } else {
                0.0
            };
        if weighted <= 0.0 {
            continue;
        }
        let degree_hint = (project.files[a].afferent + project.files[a].efferent)
            .max(project.files[b].afferent + project.files[b].efferent)
            .max(1) as f64;
        let damped = weighted / degree_hint.sqrt();
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
        generated_at_unix: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        members,
        cut_scores: cuts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphstore::model::{
        FileEntry, GraphStats, PROJECT_GRAPH_SCHEMA, Structure, SymbolEdge,
    };

    fn graph(edges: &[(&str, &str)]) -> ProjectGraph {
        let files = ["a.rs", "b.rs", "c.rs", "d.rs"]
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
}
