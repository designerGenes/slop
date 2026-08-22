use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::tags::{Tag, TagKind};

/// A definition tag carrying its own importance score.
///
/// Before, every definition in a file inherited that file's PageRank verbatim,
/// so all symbols in a file tied and the renderer fell back to line order.
/// `rank` is now distributed per identifier, so `pub fn run_deslop` and a
/// private test helper in the same file no longer score identically.
pub struct RankedTag {
    pub rank: f64,
    pub tag: Tag,
}

/// Per-file coupling numbers, in Robert C. Martin's vocabulary.
///
/// `afferent` (Ca) counts distinct files that reference something defined here;
/// `efferent` (Ce) counts distinct files this one reaches into. `instability`
/// is `Ce / (Ca + Ce)`: 0.0 means everything leans on this file and it leans on
/// nothing (change is risky), 1.0 means nothing depends on it (change is cheap).
#[derive(Debug, Clone)]
pub struct FileMetrics {
    pub rel_fname: String,
    pub rank: f64,
    pub afferent: usize,
    pub efferent: usize,
    pub instability: f64,
    pub def_count: usize,
}

impl FileMetrics {
    /// Coarse change-risk bucket, derived only from fan-in.
    pub fn risk(&self) -> &'static str {
        match self.afferent {
            0 => "none",
            1..=2 => "low",
            3..=7 => "medium",
            _ => "high",
        }
    }
}

/// One directed file-to-file dependency, with the identifiers that justify it.
#[derive(Debug, Clone)]
pub struct DependencyEdge {
    pub from: String,
    pub to: String,
    pub weight: f64,
    pub idents: Vec<String>,
}

/// Everything the renderer needs to describe the repository.
pub struct GraphAnalysis {
    pub ranked_tags: Vec<RankedTag>,
    pub metrics: Vec<FileMetrics>,
    pub edges: Vec<DependencyEdge>,
    /// Strongly connected components of size >= 2: mutual dependency knots.
    pub cycles: Vec<Vec<String>>,
    /// Articulation points of the undirected projection: removing one of these
    /// splits the dependency graph, so they are integration chokepoints.
    pub chokepoints: Vec<String>,
    /// Files nothing references. Safe to excise or rewrite freely.
    pub orphans: Vec<String>,
}

/// Identifiers defined in more than this many files are treated as generic
/// (`new`, `build`, `len`, ...). Their edges still exist but are damped, so a
/// universally reimplemented method name cannot dominate the ranking.
const AMBIGUOUS_DEF_THRESHOLD: usize = 3;

/// Cap on reported cycle size. A component larger than this is a whole-crate
/// knot; naming every member is noise, so the renderer summarises instead.
pub const MAX_REPORTED_CYCLE: usize = 12;

pub fn analyze(
    all_tags: &[Tag],
    chat_rel_fnames: &HashSet<String>,
    mentioned_idents: &HashSet<String>,
    mentioned_fnames: &HashSet<String>,
) -> GraphAnalysis {
    let mut defines: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut ref_counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut all_rel_fnames: BTreeSet<String> = BTreeSet::new();
    let mut def_counts: BTreeMap<String, usize> = BTreeMap::new();

    for tag in all_tags {
        all_rel_fnames.insert(tag.rel_fname.clone());
        match tag.kind {
            TagKind::Def => {
                defines
                    .entry(tag.name.clone())
                    .or_default()
                    .insert(tag.rel_fname.clone());
                *def_counts.entry(tag.rel_fname.clone()).or_insert(0) += 1;
            }
            TagKind::Ref => {
                *ref_counts
                    .entry(tag.name.clone())
                    .or_default()
                    .entry(tag.rel_fname.clone())
                    .or_insert(0) += 1;
            }
        }
    }

    if all_rel_fnames.is_empty() {
        return GraphAnalysis {
            ranked_tags: Vec::new(),
            metrics: Vec::new(),
            edges: Vec::new(),
            cycles: Vec::new(),
            chokepoints: Vec::new(),
            orphans: Vec::new(),
        };
    }

    // Edges point referencer -> definer, so rank accumulates on depended-upon
    // files. Weight is sqrt(reference count) rather than a flat 1.0: the old
    // code collapsed refs into a BTreeSet and lost multiplicity entirely, so a
    // file calling into another 50 times looked identical to one calling once.
    let mut edge_weights: BTreeMap<(String, String), f64> = BTreeMap::new();
    let mut edge_idents: BTreeMap<(String, String), BTreeMap<String, f64>> = BTreeMap::new();

    for (name, per_file) in &ref_counts {
        let Some(defs) = defines.get(name) else {
            continue;
        };
        if defs.is_empty() {
            continue;
        }
        // Damp identifiers that many files define; they carry little signal.
        let ambiguity = if defs.len() > AMBIGUOUS_DEF_THRESHOLD {
            1.0 / defs.len() as f64
        } else {
            1.0
        };
        for (ref_fname, count) in per_file {
            let weight = (*count as f64).sqrt() * ambiguity;
            for def_fname in defs {
                if ref_fname == def_fname {
                    continue;
                }
                let key = (ref_fname.clone(), def_fname.clone());
                *edge_weights.entry(key.clone()).or_insert(0.0) += weight;
                *edge_idents
                    .entry(key)
                    .or_default()
                    .entry(name.clone())
                    .or_insert(0.0) += weight;
            }
        }
    }

    let mut personalization: BTreeMap<String, f64> = BTreeMap::new();
    for rel_fname in chat_rel_fnames {
        if all_rel_fnames.contains(rel_fname) {
            personalization.insert(rel_fname.clone(), 100.0);
        }
    }

    let ranks = pagerank(&edge_weights, &personalization, &all_rel_fnames);

    // Spread each file's rank across the identifiers its dependents actually
    // used. A symbol nobody references keeps a small floor share of its file's
    // rank so it still appears, just below the ones that carry real traffic.
    let mut out_strength: BTreeMap<String, f64> = BTreeMap::new();
    for ((src, _), weight) in &edge_weights {
        *out_strength.entry(src.clone()).or_insert(0.0) += weight;
    }

    let mut symbol_rank: BTreeMap<(String, String), f64> = BTreeMap::new();
    for ((src, dst), weight) in &edge_weights {
        let strength = out_strength.get(src).copied().unwrap_or(0.0);
        if strength <= 0.0 {
            continue;
        }
        let share = ranks.get(src).copied().unwrap_or(0.0) * (weight / strength);
        let Some(idents) = edge_idents.get(&(src.clone(), dst.clone())) else {
            continue;
        };
        let total: f64 = idents.values().sum();
        if total <= 0.0 {
            continue;
        }
        for (ident, ident_weight) in idents {
            *symbol_rank
                .entry((dst.clone(), ident.clone()))
                .or_insert(0.0) += share * (ident_weight / total);
        }
    }

    let mut ranked_tags = Vec::new();
    for tag in all_tags {
        if tag.kind != TagKind::Def {
            continue;
        }
        let file_rank = ranks.get(&tag.rel_fname).copied().unwrap_or(0.0);
        let defs_here = def_counts.get(&tag.rel_fname).copied().unwrap_or(1).max(1);
        // Floor keeps unreferenced definitions ordered beneath referenced ones
        // without dropping them from the map entirely.
        let floor = file_rank / (defs_here as f64 * 100.0);
        let earned = symbol_rank
            .get(&(tag.rel_fname.clone(), tag.name.clone()))
            .copied()
            .unwrap_or(0.0);

        let mut boost = 1.0;
        if mentioned_idents.contains(&tag.name) {
            boost *= 10.0;
        }
        if mentioned_fnames.contains(&tag.rel_fname) {
            boost *= 5.0;
        }

        ranked_tags.push(RankedTag {
            rank: (earned + floor) * boost,
            tag: tag.clone(),
        });
    }

    ranked_tags.sort_by(|a, b| {
        b.rank
            .partial_cmp(&a.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.tag.rel_fname.cmp(&b.tag.rel_fname))
            .then_with(|| a.tag.line.cmp(&b.tag.line))
            .then_with(|| a.tag.name.cmp(&b.tag.name))
    });

    let mut afferent: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut efferent: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (src, dst) in edge_weights.keys() {
        efferent.entry(src.clone()).or_default().insert(dst.clone());
        afferent.entry(dst.clone()).or_default().insert(src.clone());
    }

    let metrics: Vec<FileMetrics> = all_rel_fnames
        .iter()
        .map(|rel| {
            let ca = afferent.get(rel).map_or(0, BTreeSet::len);
            let ce = efferent.get(rel).map_or(0, BTreeSet::len);
            let instability = if ca + ce == 0 {
                1.0
            } else {
                ce as f64 / (ca + ce) as f64
            };
            FileMetrics {
                rel_fname: rel.clone(),
                rank: ranks.get(rel).copied().unwrap_or(0.0),
                afferent: ca,
                efferent: ce,
                instability,
                def_count: def_counts.get(rel).copied().unwrap_or(0),
            }
        })
        .collect();

    let mut edges: Vec<DependencyEdge> = edge_weights
        .iter()
        .map(|((src, dst), weight)| {
            let mut idents: Vec<(String, f64)> = edge_idents
                .get(&(src.clone(), dst.clone()))
                .map(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect())
                .unwrap_or_default();
            idents.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            DependencyEdge {
                from: src.clone(),
                to: dst.clone(),
                weight: *weight,
                idents: idents.into_iter().take(4).map(|(k, _)| k).collect(),
            }
        })
        .collect();
    edges.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.from.cmp(&b.from))
            .then_with(|| a.to.cmp(&b.to))
    });

    let orphans: Vec<String> = metrics
        .iter()
        .filter(|m| m.afferent == 0 && m.def_count > 0)
        .map(|m| m.rel_fname.clone())
        .collect();

    let cycles = strongly_connected_components(&all_rel_fnames, &edge_weights);
    let chokepoints = articulation_points(&all_rel_fnames, &edge_weights);

    let mut metrics = metrics;
    metrics.sort_by(|a, b| {
        b.rank
            .partial_cmp(&a.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.rel_fname.cmp(&b.rel_fname))
    });

    GraphAnalysis {
        ranked_tags,
        metrics,
        edges,
        cycles,
        chokepoints,
        orphans,
    }
}

/// Backwards-compatible entry point: rank definitions only.
pub fn build_and_rank(
    all_tags: &[Tag],
    chat_rel_fnames: &HashSet<String>,
    mentioned_idents: &HashSet<String>,
    mentioned_fnames: &HashSet<String>,
) -> Vec<RankedTag> {
    analyze(
        all_tags,
        chat_rel_fnames,
        mentioned_idents,
        mentioned_fnames,
    )
    .ranked_tags
}

fn adjacency(
    edge_weights: &BTreeMap<(String, String), f64>,
) -> BTreeMap<String, Vec<(String, f64)>> {
    let mut adj: BTreeMap<String, Vec<(String, f64)>> = BTreeMap::new();
    for ((src, dst), weight) in edge_weights {
        adj.entry(src.clone())
            .or_default()
            .push((dst.clone(), *weight));
    }
    adj
}

fn pagerank(
    edge_weights: &BTreeMap<(String, String), f64>,
    personalization: &BTreeMap<String, f64>,
    all_nodes: &BTreeSet<String>,
) -> BTreeMap<String, f64> {
    let n = all_nodes.len();
    if n == 0 {
        return BTreeMap::new();
    }

    let damping = 0.85_f64;
    let max_iter = 100;
    let tol = 1e-6_f64;

    let total_pers: f64 = personalization.values().sum();
    let pers_norm: BTreeMap<String, f64> = if total_pers > 0.0 {
        personalization
            .iter()
            .map(|(k, v)| (k.clone(), v / total_pers))
            .collect()
    } else {
        all_nodes
            .iter()
            .map(|node| (node.clone(), 1.0 / n as f64))
            .collect()
    };

    let uniform = 1.0 / n as f64;
    let mut ranks: BTreeMap<String, f64> = all_nodes
        .iter()
        .map(|node| (node.clone(), uniform))
        .collect();

    // Adjacency list instead of rescanning every edge per node: the previous
    // implementation was O(nodes * edges) per iteration, which is quadratic on
    // any repository large enough to need a map in the first place.
    let adj = adjacency(edge_weights);
    let out_strength: BTreeMap<String, f64> = adj
        .iter()
        .map(|(node, targets)| (node.clone(), targets.iter().map(|(_, w)| w).sum()))
        .collect();

    for _ in 0..max_iter {
        let mut new_ranks: BTreeMap<String, f64> = all_nodes
            .iter()
            .map(|node| {
                let p = pers_norm.get(node).copied().unwrap_or(0.0);
                (node.clone(), (1.0 - damping) * p)
            })
            .collect();

        // Rank held by nodes with no outgoing edges is redistributed along the
        // personalization vector rather than vanishing, keeping the total at 1.
        let mut dangling = 0.0;
        for node in all_nodes {
            let rank = ranks.get(node).copied().unwrap_or(0.0);
            let strength = out_strength.get(node).copied().unwrap_or(0.0);
            if strength > 0.0 {
                if let Some(targets) = adj.get(node) {
                    for (target, weight) in targets {
                        *new_ranks.entry(target.clone()).or_insert(0.0) +=
                            damping * rank * weight / strength;
                    }
                }
            } else {
                dangling += rank;
            }
        }
        if dangling > 0.0 {
            for target in all_nodes {
                let tp = pers_norm.get(target).copied().unwrap_or(0.0);
                *new_ranks.entry(target.clone()).or_insert(0.0) += damping * dangling * tp;
            }
        }

        let mut diff = 0.0;
        for node in all_nodes {
            let old = ranks.get(node).copied().unwrap_or(0.0);
            let newv = new_ranks.get(node).copied().unwrap_or(0.0);
            diff += (newv - old).abs();
        }

        ranks = new_ranks;
        if diff < tol {
            break;
        }
    }

    ranks
}

/// Iterative Tarjan. Recursion would blow the stack on a large monorepo, so the
/// call frames are kept on an explicit work stack.
fn strongly_connected_components(
    all_nodes: &BTreeSet<String>,
    edge_weights: &BTreeMap<(String, String), f64>,
) -> Vec<Vec<String>> {
    let nodes: Vec<String> = all_nodes.iter().cloned().collect();
    let index_of: BTreeMap<&String, usize> = nodes.iter().enumerate().map(|(i, n)| (n, i)).collect();

    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    for (src, dst) in edge_weights.keys() {
        if let (Some(&s), Some(&d)) = (index_of.get(src), index_of.get(dst)) {
            adj[s].push(d);
        }
    }
    for targets in adj.iter_mut() {
        targets.sort_unstable();
        targets.dedup();
    }

    let n = nodes.len();
    let unset = usize::MAX;
    let mut index = vec![unset; n];
    let mut low = vec![0usize; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut counter = 0usize;
    let mut components = Vec::new();

    for root in 0..n {
        if index[root] != unset {
            continue;
        }
        // Each frame is (node, next-child-cursor).
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&mut (v, ref mut child)) = work.last_mut() {
            if *child == 0 {
                index[v] = counter;
                low[v] = counter;
                counter += 1;
                stack.push(v);
                on_stack[v] = true;
            }

            let mut descended = false;
            while *child < adj[v].len() {
                let w = adj[v][*child];
                *child += 1;
                if index[w] == unset {
                    work.push((w, 0));
                    descended = true;
                    break;
                } else if on_stack[w] {
                    low[v] = low[v].min(index[w]);
                }
            }
            if descended {
                continue;
            }

            if low[v] == index[v] {
                let mut component = Vec::new();
                while let Some(w) = stack.pop() {
                    on_stack[w] = false;
                    component.push(nodes[w].clone());
                    if w == v {
                        break;
                    }
                }
                if component.len() > 1 {
                    component.sort();
                    components.push(component);
                }
            }

            work.pop();
            if let Some(&mut (parent, _)) = work.last_mut() {
                low[parent] = low[parent].min(low[v]);
            }
        }
    }

    components.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    components
}

/// Articulation points of the undirected projection, found iteratively.
/// A file here is a structural chokepoint: cut it and the graph falls apart.
fn articulation_points(
    all_nodes: &BTreeSet<String>,
    edge_weights: &BTreeMap<(String, String), f64>,
) -> Vec<String> {
    let nodes: Vec<String> = all_nodes.iter().cloned().collect();
    let index_of: BTreeMap<&String, usize> = nodes.iter().enumerate().map(|(i, n)| (n, i)).collect();
    let n = nodes.len();

    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (src, dst) in edge_weights.keys() {
        if let (Some(&s), Some(&d)) = (index_of.get(src), index_of.get(dst)) {
            adj[s].push(d);
            adj[d].push(s);
        }
    }
    for targets in adj.iter_mut() {
        targets.sort_unstable();
        targets.dedup();
    }

    let unset = usize::MAX;
    let mut disc = vec![unset; n];
    let mut low = vec![0usize; n];
    let mut parent = vec![unset; n];
    let mut is_ap = vec![false; n];
    let mut timer = 0usize;

    for root in 0..n {
        if disc[root] != unset {
            continue;
        }
        let mut root_children = 0usize;
        let mut work: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&mut (u, ref mut child)) = work.last_mut() {
            if *child == 0 {
                disc[u] = timer;
                low[u] = timer;
                timer += 1;
            }

            let mut descended = false;
            while *child < adj[u].len() {
                let v = adj[u][*child];
                *child += 1;
                if disc[v] == unset {
                    parent[v] = u;
                    if u == root {
                        root_children += 1;
                    }
                    work.push((v, 0));
                    descended = true;
                    break;
                } else if v != parent[u] {
                    low[u] = low[u].min(disc[v]);
                }
            }
            if descended {
                continue;
            }

            work.pop();
            if let Some(&mut (p, _)) = work.last_mut() {
                low[p] = low[p].min(low[u]);
                if p != root && low[u] >= disc[p] {
                    is_ap[p] = true;
                }
            }
        }
        if root_children > 1 {
            is_ap[root] = true;
        }
    }

    let mut out: Vec<String> = (0..n).filter(|&i| is_ap[i]).map(|i| nodes[i].clone()).collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repomap::tags::{Tag, TagKind};

    fn def(file: &str, line: usize, name: &str) -> Tag {
        Tag { rel_fname: file.into(), line, name: name.into(), kind: TagKind::Def }
    }
    fn r#ref(file: &str, line: usize, name: &str) -> Tag {
        Tag { rel_fname: file.into(), line, name: name.into(), kind: TagKind::Ref }
    }

    fn empty() -> HashSet<String> { HashSet::new() }

    #[test]
    fn symbols_in_one_file_no_longer_tie() {
        // `hot` is called from two files, `cold` from none. Before this change
        // both inherited core.rs's file rank and sorted only by line number.
        let tags = vec![
            def("core.rs", 1, "hot"),
            def("core.rs", 9, "cold"),
            r#ref("a.rs", 2, "hot"),
            r#ref("b.rs", 3, "hot"),
        ];
        let ranked = build_and_rank(&tags, &empty(), &empty(), &empty());
        let hot = ranked.iter().find(|r| r.tag.name == "hot").expect("hot");
        let cold = ranked.iter().find(|r| r.tag.name == "cold").expect("cold");
        assert!(hot.rank > cold.rank, "hot {} cold {}", hot.rank, cold.rank);
    }

    #[test]
    fn reference_multiplicity_changes_edge_weight() {
        let light = vec![def("core.rs", 1, "f"), r#ref("a.rs", 1, "f")];
        let heavy = vec![
            def("core.rs", 1, "f"),
            r#ref("a.rs", 1, "f"),
            r#ref("a.rs", 2, "f"),
            r#ref("a.rs", 3, "f"),
            r#ref("a.rs", 4, "f"),
        ];
        let light_w = analyze(&light, &empty(), &empty(), &empty()).edges[0].weight;
        let heavy_w = analyze(&heavy, &empty(), &empty(), &empty()).edges[0].weight;
        assert!(heavy_w > light_w, "heavy {heavy_w} light {light_w}");
    }

    #[test]
    fn coupling_metrics_identify_stable_and_removable_files() {
        let tags = vec![
            def("error.rs", 1, "SlopError"),
            def("a.rs", 1, "run_a"),
            def("b.rs", 1, "run_b"),
            def("dead.rs", 1, "unused"),
            r#ref("a.rs", 2, "SlopError"),
            r#ref("b.rs", 2, "SlopError"),
            r#ref("dead.rs", 2, "SlopError"),
        ];
        let analysis = analyze(&tags, &empty(), &empty(), &empty());
        let by = |n: &str| {
            analysis.metrics.iter().find(|m| m.rel_fname == n).expect(n).clone()
        };

        let error = by("error.rs");
        assert_eq!(error.afferent, 3);
        assert_eq!(error.efferent, 0);
        assert_eq!(error.instability, 0.0);
        assert_eq!(error.risk(), "medium");

        let dead = by("dead.rs");
        assert_eq!(dead.afferent, 0);
        assert_eq!(dead.instability, 1.0);
        assert_eq!(dead.risk(), "none");
        assert!(analysis.orphans.contains(&"dead.rs".to_string()));
        assert!(!analysis.orphans.contains(&"error.rs".to_string()));
    }

    #[test]
    fn detects_mutual_dependency_cycle() {
        let tags = vec![
            def("a.rs", 1, "alpha"),
            def("b.rs", 1, "beta"),
            r#ref("a.rs", 2, "beta"),
            r#ref("b.rs", 2, "alpha"),
        ];
        let analysis = analyze(&tags, &empty(), &empty(), &empty());
        assert_eq!(analysis.cycles.len(), 1);
        assert_eq!(analysis.cycles[0], vec!["a.rs".to_string(), "b.rs".to_string()]);
    }

    #[test]
    fn acyclic_graph_reports_no_cycles() {
        let tags = vec![
            def("core.rs", 1, "core_fn"),
            def("leaf.rs", 1, "leaf_fn"),
            r#ref("leaf.rs", 2, "core_fn"),
        ];
        assert!(analyze(&tags, &empty(), &empty(), &empty()).cycles.is_empty());
    }

    #[test]
    fn finds_the_chokepoint_between_two_clusters() {
        // a - hub - b: hub is the only path between the two halves.
        let tags = vec![
            def("hub.rs", 1, "hub_fn"),
            def("a.rs", 1, "a_fn"),
            def("b.rs", 1, "b_fn"),
            r#ref("a.rs", 2, "hub_fn"),
            r#ref("b.rs", 2, "hub_fn"),
        ];
        let analysis = analyze(&tags, &empty(), &empty(), &empty());
        assert_eq!(analysis.chokepoints, vec!["hub.rs".to_string()]);
    }

    #[test]
    fn edges_carry_the_identifiers_that_justify_them() {
        let tags = vec![
            def("core.rs", 1, "alpha"),
            def("core.rs", 2, "beta"),
            r#ref("user.rs", 1, "alpha"),
            r#ref("user.rs", 2, "beta"),
        ];
        let analysis = analyze(&tags, &empty(), &empty(), &empty());
        let edge = &analysis.edges[0];
        assert_eq!(edge.from, "user.rs");
        assert_eq!(edge.to, "core.rs");
        assert!(edge.idents.contains(&"alpha".to_string()));
        assert!(edge.idents.contains(&"beta".to_string()));
    }

    #[test]
    fn ambiguous_identifiers_are_damped() {
        // `new` defined in five files must not outweigh a specific symbol.
        let mut tags = Vec::new();
        for i in 0..5 {
            tags.push(def(&format!("m{i}.rs"), 1, "new"));
        }
        tags.push(def("specific.rs", 1, "run_deslop"));
        tags.push(r#ref("caller.rs", 1, "new"));
        tags.push(r#ref("caller.rs", 2, "run_deslop"));

        let analysis = analyze(&tags, &empty(), &empty(), &empty());
        let to_specific = analysis
            .edges
            .iter()
            .find(|e| e.to == "specific.rs")
            .expect("edge to specific.rs");
        let to_module = analysis
            .edges
            .iter()
            .find(|e| e.to == "m0.rs")
            .expect("edge to m0.rs");
        assert!(to_specific.weight > to_module.weight);
    }

    #[test]
    fn pagerank_conserves_total_mass() {
        let tags = vec![
            def("a.rs", 1, "alpha"),
            def("b.rs", 1, "beta"),
            def("island.rs", 1, "lonely"),
            r#ref("b.rs", 2, "alpha"),
        ];
        let analysis = analyze(&tags, &empty(), &empty(), &empty());
        let total: f64 = analysis.metrics.iter().map(|m| m.rank).sum();
        assert!((total - 1.0).abs() < 1e-6, "total rank was {total}");
    }

    #[test]
    fn empty_input_is_handled() {
        let analysis = analyze(&[], &empty(), &empty(), &empty());
        assert!(analysis.ranked_tags.is_empty());
        assert!(analysis.metrics.is_empty());
        assert!(analysis.cycles.is_empty());
    }
}
