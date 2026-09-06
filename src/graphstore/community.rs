//! Louvain community detection over the undirected projection of the graph.
//!
//! Directory layout is a claim about module boundaries; the edge structure is
//! evidence about them. They disagree often enough to be worth measuring: a
//! `utils/` folder usually shatters into three communities, and a feature
//! implemented across four directories usually shows up as one. Stage 2 uses
//! these clusters to decide how far the tower's attention should spread — a
//! neighbour inside the seed's own community is nearer, in the sense that
//! matters, than a neighbour one hop away in some other module.
//!
//! The implementation is deterministic: nodes are visited in index order and
//! ties are resolved by keeping the current assignment, so the same input
//! always yields the same labelling. Randomized Louvain would produce a graph
//! that changes shape between runs on an unchanged repo, which would make the
//! persisted artifact useless as a diffable record.

use std::collections::BTreeMap;

/// Weighted undirected graph over dense node indices.
///
/// Self-loops are held apart from the adjacency list rather than stored as
/// `(i, i)` entries; the modularity arithmetic needs them counted once in the
/// degree and never walked as neighbours, and keeping them separate makes both
/// impossible to get wrong.
#[derive(Debug, Clone)]
pub struct UndirectedGraph {
    pub node_count: usize,
    adjacency: Vec<Vec<(usize, f64)>>,
    self_loops: Vec<f64>,
    /// Sum of every edge weight, each undirected edge counted once.
    total_weight: f64,
}

impl UndirectedGraph {
    /// Build from an edge list. Duplicate and reversed pairs are summed, so the
    /// caller may hand over a directed edge list without pre-merging it.
    pub fn from_edges(node_count: usize, edges: &[(usize, usize, f64)]) -> Self {
        let mut merged: BTreeMap<(usize, usize), f64> = BTreeMap::new();
        let mut self_loops = vec![0.0; node_count];

        for &(from, to, weight) in edges {
            if from >= node_count || to >= node_count || weight <= 0.0 {
                continue;
            }
            if from == to {
                self_loops[from] += weight;
                continue;
            }
            let key = (from.min(to), from.max(to));
            *merged.entry(key).or_insert(0.0) += weight;
        }

        let mut adjacency = vec![Vec::new(); node_count];
        let mut total_weight: f64 = self_loops.iter().sum();
        for ((a, b), weight) in merged {
            adjacency[a].push((b, weight));
            adjacency[b].push((a, weight));
            total_weight += weight;
        }

        Self {
            node_count,
            adjacency,
            self_loops,
            total_weight,
        }
    }

    /// Weighted degree, with self-loops counted twice as the measure requires.
    fn degree(&self, node: usize) -> f64 {
        let incident: f64 = self.adjacency[node].iter().map(|(_, w)| *w).sum();
        incident + 2.0 * self.self_loops[node]
    }

    pub fn total_weight(&self) -> f64 {
        self.total_weight
    }
}

const EPSILON: f64 = 1e-12;

/// Assign every node a community index in `0..k`.
///
/// `resolution` above 1.0 yields more, smaller communities; below 1.0, fewer
/// and larger. 1.0 is standard modularity.
pub fn louvain(graph: &UndirectedGraph, resolution: f64) -> Vec<usize> {
    let n = graph.node_count;
    if n == 0 {
        return Vec::new();
    }
    if graph.total_weight <= 0.0 {
        // No edges at all: every file is its own module, which is the honest
        // answer for a repo with nothing linking it together.
        return (0..n).collect();
    }

    let mut mapping: Vec<usize> = (0..n).collect();
    let mut current = graph.clone();

    loop {
        let (labels, moved) = one_level(&current, resolution);
        for slot in mapping.iter_mut() {
            *slot = labels[*slot];
        }
        let community_count = labels.iter().copied().max().map_or(0, |max| max + 1);
        if !moved || community_count == current.node_count || community_count <= 1 {
            break;
        }
        current = aggregate(&current, &labels, community_count);
    }

    compact(&mapping)
}

/// One pass of local moving until no node improves modularity by relocating.
fn one_level(graph: &UndirectedGraph, resolution: f64) -> (Vec<usize>, bool) {
    let n = graph.node_count;
    let m2 = 2.0 * graph.total_weight;
    let degrees: Vec<f64> = (0..n).map(|node| graph.degree(node)).collect();

    let mut community: Vec<usize> = (0..n).collect();
    let mut sum_tot: Vec<f64> = degrees.clone();
    let mut moved_any = false;

    // A repo-scale graph settles well within this; the bound only exists so a
    // pathological weighting cannot spin here forever.
    for _ in 0..64 {
        let mut moved_this_pass = false;

        for node in 0..n {
            let origin = community[node];
            sum_tot[origin] -= degrees[node];

            let mut weight_to: BTreeMap<usize, f64> = BTreeMap::new();
            for &(neighbor, weight) in &graph.adjacency[node] {
                *weight_to.entry(community[neighbor]).or_insert(0.0) += weight;
            }

            let mut best_community = origin;
            let mut best_gain = weight_to.get(&origin).copied().unwrap_or(0.0)
                - resolution * degrees[node] * sum_tot[origin] / m2;

            // BTreeMap iteration is ascending, and the comparison is strict, so
            // ties fall to the lowest community index and the result does not
            // depend on hash ordering.
            for (&candidate, &weight) in &weight_to {
                let gain = weight - resolution * degrees[node] * sum_tot[candidate] / m2;
                if gain > best_gain + EPSILON {
                    best_gain = gain;
                    best_community = candidate;
                }
            }

            sum_tot[best_community] += degrees[node];
            community[node] = best_community;
            if best_community != origin {
                moved_this_pass = true;
                moved_any = true;
            }
        }

        if !moved_this_pass {
            break;
        }
    }

    (compact(&community), moved_any)
}

/// Collapse each community into a single node for the next level.
fn aggregate(graph: &UndirectedGraph, labels: &[usize], community_count: usize) -> UndirectedGraph {
    let mut edges: Vec<(usize, usize, f64)> = Vec::new();

    for node in 0..graph.node_count {
        if graph.self_loops[node] > 0.0 {
            edges.push((labels[node], labels[node], graph.self_loops[node]));
        }
        for &(neighbor, weight) in &graph.adjacency[node] {
            // Adjacency is symmetric; take each unordered pair once.
            if neighbor < node {
                continue;
            }
            edges.push((labels[node], labels[neighbor], weight));
        }
    }

    UndirectedGraph::from_edges(community_count, &edges)
}

/// Renumber arbitrary labels to `0..k` in order of first appearance.
fn compact(labels: &[usize]) -> Vec<usize> {
    let mut seen: BTreeMap<usize, usize> = BTreeMap::new();
    let mut out = Vec::with_capacity(labels.len());
    for &label in labels {
        let next = seen.len();
        out.push(*seen.entry(label).or_insert(next));
    }
    out
}

/// Modularity of a labelling, for tests and for reporting build quality.
pub fn modularity(graph: &UndirectedGraph, labels: &[usize], resolution: f64) -> f64 {
    if graph.total_weight <= 0.0 {
        return 0.0;
    }
    let m2 = 2.0 * graph.total_weight;
    let count = labels.iter().copied().max().map_or(0, |max| max + 1);

    let mut internal = vec![0.0; count];
    let mut total = vec![0.0; count];

    for node in 0..graph.node_count {
        let community = labels[node];
        total[community] += graph.degree(node);
        internal[community] += 2.0 * graph.self_loops[node];
        for &(neighbor, weight) in &graph.adjacency[node] {
            if labels[neighbor] == community {
                internal[community] += weight;
            }
        }
    }

    (0..count)
        .map(|c| internal[c] / m2 - resolution * (total[c] / m2).powi(2))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two triangles joined by a single edge: the textbook case where the right
    /// answer is unambiguous and any working implementation must find it.
    fn barbell() -> UndirectedGraph {
        UndirectedGraph::from_edges(
            6,
            &[
                (0, 1, 1.0),
                (1, 2, 1.0),
                (0, 2, 1.0),
                (3, 4, 1.0),
                (4, 5, 1.0),
                (3, 5, 1.0),
                (2, 3, 1.0),
            ],
        )
    }

    #[test]
    fn splits_a_barbell_into_its_two_lobes() {
        let graph = barbell();
        let labels = louvain(&graph, 1.0);
        assert_eq!(labels[0], labels[1]);
        assert_eq!(labels[1], labels[2]);
        assert_eq!(labels[3], labels[4]);
        assert_eq!(labels[4], labels[5]);
        assert_ne!(labels[0], labels[5]);
    }

    #[test]
    fn is_deterministic_across_runs() {
        let graph = barbell();
        let first = louvain(&graph, 1.0);
        for _ in 0..8 {
            assert_eq!(louvain(&graph, 1.0), first);
        }
    }

    #[test]
    fn isolated_nodes_each_get_their_own_community() {
        let graph = UndirectedGraph::from_edges(4, &[]);
        let labels = louvain(&graph, 1.0);
        assert_eq!(labels, vec![0, 1, 2, 3]);
    }

    #[test]
    fn labels_are_dense_and_zero_based() {
        let labels = louvain(&barbell(), 1.0);
        let max = labels.iter().copied().max().expect("labels");
        let distinct: std::collections::BTreeSet<usize> = labels.iter().copied().collect();
        assert_eq!(distinct.len(), max + 1);
        assert!(distinct.contains(&0));
    }

    #[test]
    fn reversed_and_duplicate_edges_are_merged_not_double_counted() {
        let single = UndirectedGraph::from_edges(2, &[(0, 1, 2.0)]);
        let doubled = UndirectedGraph::from_edges(2, &[(0, 1, 1.0), (1, 0, 1.0)]);
        assert!((single.total_weight() - doubled.total_weight()).abs() < EPSILON);
    }

    #[test]
    fn the_split_scores_better_than_lumping_everything_together() {
        let graph = barbell();
        let found = modularity(&graph, &louvain(&graph, 1.0), 1.0);
        let lumped = modularity(&graph, &vec![0; 6], 1.0);
        assert!(found > lumped, "found {found}, lumped {lumped}");
    }

    #[test]
    fn a_higher_resolution_does_not_produce_fewer_communities() {
        let graph = barbell();
        let coarse = louvain(&graph, 0.5).iter().copied().max().expect("max");
        let fine = louvain(&graph, 2.0).iter().copied().max().expect("max");
        assert!(fine >= coarse);
    }
}
