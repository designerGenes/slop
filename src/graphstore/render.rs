//! Rendering a stored graph as something a person or an agent can read.
//!
//! The JSON in the cache is the source of truth; this is the view. It is
//! deliberately the same dialect as the `-g` repomap block — same column
//! vocabulary, same `#SLOP_REQUEST` convention — so an agent that has learned
//! to read one can read the other without new instructions.

use std::collections::BTreeMap;

use super::model::{BuildReport, ProjectGraph};
use super::tower::{Tier, TowerGraph};

pub fn render_tower(tower: &TowerGraph, project: &ProjectGraph) -> String {
    let mut out = format!(
        "# TOWER GRAPH\n# repo: {}\n# seeds: {}\n#\n",
        project.repo_root,
        tower.seeds.join(", ")
    );
    for (tier, label) in [
        (Tier::Zero, "TIER 0"),
        (Tier::One, "TIER 1"),
        (Tier::Two, "TIER 2"),
        (Tier::Three, "TIER 3"),
    ] {
        out.push_str(&format!("## {label}\n\n"));
        for member in tower.members.iter().filter(|member| member.tier == tier) {
            let via = if member.via.is_empty() {
                String::new()
            } else {
                format!(" via {}", member.via.join(", "))
            };
            out.push_str(&format!("  {:.6}  {}{}\n", member.score, member.rel, via));
        }
        out.push('\n');
    }
    out
}

const MAX_MODULE_MEMBERS: usize = 24;
const MAX_METRIC_ROWS: usize = 60;
const MAX_DEPENDENCY_ROWS: usize = 60;
const MAX_COCHANGE_ROWS: usize = 40;
const MAX_STRUCTURE_ROWS: usize = 24;

pub fn render(graph: &ProjectGraph, report: Option<&BuildReport>) -> String {
    let mut out = String::new();
    out.push_str(&render_header(graph, report));
    out.push_str(&render_modules(graph));
    out.push_str(&render_metrics(graph));
    out.push_str(&render_dependencies(graph));
    out.push_str(&render_cochange(graph));
    out.push_str(&render_structure(graph));
    out
}

fn render_header(graph: &ProjectGraph, report: Option<&BuildReport>) -> String {
    let stats = &graph.stats;
    let mut out = String::new();
    out.push_str("# PROJECT GRAPH\n");
    out.push_str(&format!("# repo: {}\n", graph.repo_root));
    out.push_str(&format!(
        "# id: {}  generator: slop {}\n",
        graph.repo_id, graph.generator_version
    ));
    out.push_str(&format!(
        "# files: {} ({} parsed){}  symbol-edges: {}  co-change-edges: {}\n",
        stats.file_count,
        stats.parsed_count,
        if stats.manifest_truncated > 0 {
            format!("  [{} beyond the walk ceiling]", stats.manifest_truncated)
        } else {
            String::new()
        },
        stats.symbol_edge_count,
        stats.cochange_edge_count,
    ));
    out.push_str(&format!(
        "# modules: {} (modularity {:.3}){}\n",
        stats.community_count,
        stats.modularity,
        if stats.from_git {
            ""
        } else {
            "  [manifest from directory walk, not git]"
        },
    ));
    if let Some(report) = report {
        out.push_str(&format!("# build: {}\n", report.summary()));
    }
    out.push_str("#\n");
    out.push_str("# Sections: MODULES (edge-derived clusters), METRICS (per-file coupling),\n");
    out.push_str("# DEPENDENCIES (who uses whom), CO-CHANGE (what ships together),\n");
    out.push_str("# STRUCTURE (cycles, chokepoints, dead ends).\n");
    out.push_str("# Ca = files depending on this one. Ce = files this one depends on.\n");
    out.push_str("# I  = Ce/(Ca+Ce). Near 0 = heavily depended upon, changes are risky.\n");
    out.push_str("# Modules come from the edges, not the directory tree; when the two\n");
    out.push_str("# disagree, the edges are describing how the code actually behaves.\n");
    out.push_str("# Request any file with:  #SLOP_REQUEST \"<absolute path>\" <reason>\n");
    out
}

fn render_modules(graph: &ProjectGraph) -> String {
    if graph.communities.is_empty() {
        return String::new();
    }

    // Singletons are the common case in any repo with loose files hanging off
    // the root; listing each of them as its own module would bury the ones that
    // mean something.
    let mut ranked: Vec<&super::model::Community> = graph
        .communities
        .iter()
        .filter(|community| community.members.len() > 1)
        .collect();
    ranked.sort_by(|left, right| {
        right
            .members
            .len()
            .cmp(&left.members.len())
            .then_with(|| left.label.cmp(&right.label))
    });

    let singletons = graph.communities.len() - ranked.len();

    let mut out = String::from("\n## MODULES\n\n");
    for community in &ranked {
        let cohesion = cohesion(community.internal_weight, community.external_weight);
        out.push_str(&format!(
            "  [{}] {} — {} files, cohesion {:.2}\n",
            community.id,
            community.label,
            community.members.len(),
            cohesion
        ));
        for member in community.members.iter().take(MAX_MODULE_MEMBERS) {
            out.push_str(&format!("       {member}\n"));
        }
        if community.members.len() > MAX_MODULE_MEMBERS {
            out.push_str(&format!(
                "       ... {} more\n",
                community.members.len() - MAX_MODULE_MEMBERS
            ));
        }
    }
    if singletons > 0 {
        out.push_str(&format!(
            "\n  ({singletons} file(s) belong to no module: nothing links them to anything)\n"
        ));
    }
    out
}

/// Share of a module's edge weight that stays inside it. 1.0 is a sealed unit.
fn cohesion(internal: f64, external: f64) -> f64 {
    let total = internal + external;
    if total <= 0.0 { 0.0 } else { internal / total }
}

fn render_metrics(graph: &ProjectGraph) -> String {
    let mut ranked: Vec<&super::model::FileEntry> = graph
        .files
        .iter()
        .filter(|file| file.parsed || file.afferent > 0 || file.efferent > 0)
        .collect();
    if ranked.is_empty() {
        return String::new();
    }
    ranked.sort_by(|left, right| {
        right
            .rank
            .partial_cmp(&left.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.rel.cmp(&right.rel))
    });

    let mut out = String::from("\n## METRICS\n\n");
    out.push_str("     rank      Ca   Ce     I  risk    mod  file\n");
    for file in ranked.iter().take(MAX_METRIC_ROWS) {
        out.push_str(&format!(
            "  {:.5}  {:>4} {:>4}  {:.2}  {:<6} {:>4}  {}\n",
            file.rank,
            file.afferent,
            file.efferent,
            file.instability,
            file.risk(),
            file.community
                .map(|id| id.to_string())
                .unwrap_or_else(|| "-".to_string()),
            file.rel
        ));
    }
    if ranked.len() > MAX_METRIC_ROWS {
        out.push_str(&format!(
            "  ... {} more file(s)\n",
            ranked.len() - MAX_METRIC_ROWS
        ));
    }
    out
}

fn render_dependencies(graph: &ProjectGraph) -> String {
    if graph.symbol_edges.is_empty() {
        return String::new();
    }
    let mut edges: Vec<&super::model::SymbolEdge> = graph.symbol_edges.iter().collect();
    edges.sort_by(|left, right| {
        right
            .weight
            .partial_cmp(&left.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.from.cmp(&right.from))
            .then_with(|| left.to.cmp(&right.to))
    });

    let mut out = String::from("\n## DEPENDENCIES\n\n");
    for edge in edges.iter().take(MAX_DEPENDENCY_ROWS) {
        let via = if edge.idents.is_empty() {
            String::new()
        } else {
            format!("  via {}", edge.idents.join(", "))
        };
        out.push_str(&format!(
            "  {} -> {}  ({:.2}){}\n",
            edge.from, edge.to, edge.weight, via
        ));
    }
    if edges.len() > MAX_DEPENDENCY_ROWS {
        out.push_str(&format!(
            "  ... {} more edge(s)\n",
            edges.len() - MAX_DEPENDENCY_ROWS
        ));
    }
    out
}

fn render_cochange(graph: &ProjectGraph) -> String {
    if graph.cochange_edges.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## CO-CHANGE\n\n");
    out.push_str("# Pairs that keep appearing in the same commit. No symbol edge required:\n");
    out.push_str("# this is the coupling the parser cannot see.\n");
    for edge in graph.cochange_edges.iter().take(MAX_COCHANGE_ROWS) {
        let unlinked = !has_symbol_edge(graph, &edge.a, &edge.b);
        out.push_str(&format!(
            "  {} <-> {}  ({} commits, {:.2}){}\n",
            edge.a,
            edge.b,
            edge.commits,
            edge.weight,
            if unlinked { "  [no symbol edge]" } else { "" }
        ));
    }
    if graph.cochange_edges.len() > MAX_COCHANGE_ROWS {
        out.push_str(&format!(
            "  ... {} more pair(s)\n",
            graph.cochange_edges.len() - MAX_COCHANGE_ROWS
        ));
    }
    out
}

fn has_symbol_edge(graph: &ProjectGraph, a: &str, b: &str) -> bool {
    graph
        .symbol_edges
        .iter()
        .any(|edge| (edge.from == a && edge.to == b) || (edge.from == b && edge.to == a))
}

fn render_structure(graph: &ProjectGraph) -> String {
    let structure = &graph.structure;
    if structure.cycles.is_empty()
        && structure.chokepoints.is_empty()
        && structure.orphans.is_empty()
    {
        return String::new();
    }

    let mut out = String::from("\n## STRUCTURE\n\n");

    if !structure.cycles.is_empty() {
        out.push_str("  cycles (mutual dependency, change one and you change all):\n");
        for cycle in structure.cycles.iter().take(MAX_STRUCTURE_ROWS) {
            if cycle.len() > crate::repomap::graph::MAX_REPORTED_CYCLE {
                out.push_str(&format!(
                    "    {} file knot including {}\n",
                    cycle.len(),
                    cycle.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                ));
            } else {
                out.push_str(&format!("    {}\n", cycle.join(" <-> ")));
            }
        }
    }

    if !structure.chokepoints.is_empty() {
        out.push_str("  chokepoints (removing one splits the graph):\n");
        out.push_str(&format!(
            "    {}\n",
            summarize(&structure.chokepoints, MAX_STRUCTURE_ROWS)
        ));
    }

    if !structure.orphans.is_empty() {
        out.push_str("  orphans (nothing references these):\n");
        out.push_str(&format!(
            "    {}\n",
            summarize(&structure.orphans, MAX_STRUCTURE_ROWS)
        ));
    }

    out
}

fn summarize(items: &[String], limit: usize) -> String {
    if items.len() <= limit {
        return items.join(", ");
    }
    format!(
        "{}, ... {} more",
        items[..limit].join(", "),
        items.len() - limit
    )
}

/// One line per file, for the terminal summary the command prints.
pub fn module_overview(graph: &ProjectGraph) -> BTreeMap<String, usize> {
    let mut out = BTreeMap::new();
    for community in &graph.communities {
        if community.members.len() > 1 {
            out.insert(community.label.clone(), community.members.len());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphstore::model::{
        CoChangeEdge, Community, FileEntry, GraphStats, PROJECT_GRAPH_SCHEMA, ProjectGraph,
        Structure, SymbolEdge,
    };

    fn file(rel: &str, rank: f64, community: usize) -> FileEntry {
        FileEntry {
            rel: rel.to_string(),
            blake3: "0".repeat(64),
            size: 10,
            parsed: true,
            tags: Vec::new(),
            rank,
            afferent: 2,
            efferent: 1,
            instability: 0.33,
            def_count: 3,
            community: Some(community),
        }
    }

    fn graph() -> ProjectGraph {
        ProjectGraph {
            schema: PROJECT_GRAPH_SCHEMA,
            repo_root: "/tmp/demo".to_string(),
            repo_id: "demo-0123456789abcdef".to_string(),
            generated_at_unix: 0,
            generator_version: "0.3.0".to_string(),
            files: vec![file("src/a.rs", 0.4, 0), file("src/b.rs", 0.2, 0)],
            symbol_edges: vec![SymbolEdge {
                from: "src/a.rs".to_string(),
                to: "src/b.rs".to_string(),
                weight: 2.5,
                idents: vec!["run".to_string()],
            }],
            cochange_edges: vec![CoChangeEdge {
                a: "src/a.rs".to_string(),
                b: "docs/a.md".to_string(),
                commits: 6,
                weight: 3.0,
            }],
            communities: vec![Community {
                id: 0,
                label: "src".to_string(),
                members: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
                internal_weight: 8.0,
                external_weight: 2.0,
            }],
            structure: Structure {
                cycles: Vec::new(),
                chokepoints: vec!["src/a.rs".to_string()],
                orphans: Vec::new(),
            },
            stats: GraphStats {
                file_count: 2,
                parsed_count: 2,
                symbol_edge_count: 1,
                cochange_edge_count: 1,
                community_count: 1,
                manifest_truncated: 0,
                from_git: true,
                modularity: 0.41,
            },
        }
    }

    #[test]
    fn renders_every_section_it_has_data_for() {
        let out = render(&graph(), None);
        assert!(out.contains("## MODULES"), "{out}");
        assert!(out.contains("## METRICS"), "{out}");
        assert!(out.contains("## DEPENDENCIES"), "{out}");
        assert!(out.contains("## CO-CHANGE"), "{out}");
        assert!(out.contains("## STRUCTURE"), "{out}");
    }

    #[test]
    fn keeps_the_request_convention_the_repomap_taught() {
        assert!(render(&graph(), None).contains("#SLOP_REQUEST"));
    }

    #[test]
    fn flags_co_change_pairs_the_symbol_graph_cannot_explain() {
        // docs/a.md has no symbol edge to src/a.rs; that is the interesting case
        // and the whole reason for mining history.
        assert!(render(&graph(), None).contains("[no symbol edge]"));
    }

    #[test]
    fn omits_sections_with_nothing_in_them() {
        let mut bare = graph();
        bare.cochange_edges.clear();
        bare.symbol_edges.clear();
        let out = render(&bare, None);
        assert!(!out.contains("## CO-CHANGE"), "{out}");
        assert!(!out.contains("## DEPENDENCIES"), "{out}");
    }

    #[test]
    fn a_sealed_module_scores_full_cohesion() {
        assert!((cohesion(4.0, 0.0) - 1.0).abs() < 1e-9);
        assert!((cohesion(0.0, 0.0)).abs() < 1e-9);
    }

    #[test]
    fn reports_the_build_when_one_is_supplied() {
        let report = BuildReport {
            reparsed: 2,
            reused: 40,
            ..BuildReport::default()
        };
        assert!(render(&graph(), Some(&report)).contains("40 reused"));
    }
}
