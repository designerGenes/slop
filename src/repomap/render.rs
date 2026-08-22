use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::graph::{DependencyEdge, FileMetrics, RankedTag, MAX_REPORTED_CYCLE};
use super::manifest::{self, Manifest};

/// Legend emitted once, so the consuming agent can read the rest without
/// guessing what the columns mean.
pub const LEGEND: &str = "\
# REPO GRAPH
# Sections: MANIFEST (every file in the repo), METRICS (per-file coupling),
# DEPENDENCIES (who uses whom), STRUCTURE (cycles, chokepoints, dead ends),
# SYMBOLS (ranked definitions).
# Manifest marks: * important/config  ~ symbols parsed  @ full text included in this slop
# Metrics: rank = personalized PageRank over the file dependency graph.
#   Ca = files depending on this one. Ce = files this one depends on.
#   I  = Ce/(Ca+Ce). I near 0 = heavily depended upon, changes are risky.
#                    I near 1 = depends on others, nothing depends on it, cheap to change.
#   risk = change blast radius, bucketed from Ca.
# Files marked @ are already included in full below; the SYMBOLS section skips
# them deliberately and spends its budget on files you cannot otherwise see.
# Request any file with:  #SLOP_REQUEST \"<absolute path>\" <reason>";

pub fn render_manifest(manifest: &Manifest, in_bundle: &BTreeSet<String>) -> String {
    let source = if manifest.from_git {
        "git ls-files"
    } else {
        "filesystem walk"
    };
    let shown = manifest.files.len();
    let total = shown + manifest.truncated;
    format!(
        "\n## MANIFEST ({shown} of {total} files, via {source})\n\n{}",
        manifest::render_tree(manifest, in_bundle)
    )
}

pub fn render_metrics(metrics: &[FileMetrics], limit: usize) -> String {
    if metrics.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## METRICS\n\n");
    out.push_str("  rank      Ca   Ce     I  risk    file\n");
    for m in metrics.iter().take(limit) {
        out.push_str(&format!(
            "  {:<8.5}  {:>3}  {:>3}  {:>4.2}  {:<6}  {}\n",
            m.rank,
            m.afferent,
            m.efferent,
            m.instability,
            m.risk(),
            m.rel_fname
        ));
    }
    if metrics.len() > limit {
        out.push_str(&format!(
            "  ... {} further files omitted\n",
            metrics.len() - limit
        ));
    }
    out
}

pub fn render_dependencies(edges: &[DependencyEdge], limit: usize) -> String {
    if edges.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## DEPENDENCIES\n\n");
    for edge in edges.iter().take(limit) {
        let via = if edge.idents.is_empty() {
            String::new()
        } else {
            format!("  [{}]", edge.idents.join(", "))
        };
        out.push_str(&format!(
            "  {} -> {} (w {:.1}){}\n",
            edge.from, edge.to, edge.weight, via
        ));
    }
    if edges.len() > limit {
        out.push_str(&format!(
            "  ... {} further edges omitted\n",
            edges.len() - limit
        ));
    }
    out
}

pub fn render_structure(
    cycles: &[Vec<String>],
    chokepoints: &[String],
    orphans: &[String],
) -> String {
    if cycles.is_empty() && chokepoints.is_empty() && orphans.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n## STRUCTURE\n\n");

    if cycles.is_empty() {
        out.push_str("  cycles: none\n");
    } else {
        for cycle in cycles {
            if cycle.len() > MAX_REPORTED_CYCLE {
                out.push_str(&format!(
                    "  cycle: {} files mutually dependent, e.g. {}, ...\n",
                    cycle.len(),
                    cycle
                        .iter()
                        .take(4)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            } else {
                out.push_str(&format!("  cycle: {}\n", cycle.join(" <-> ")));
            }
        }
    }

    if chokepoints.is_empty() {
        out.push_str("  chokepoints: none\n");
    } else {
        out.push_str(&format!(
            "  chokepoints (cutting these splits the graph): {}\n",
            chokepoints.join(", ")
        ));
    }

    if orphans.is_empty() {
        out.push_str("  unreferenced: none\n");
    } else {
        out.push_str(&format!(
            "  unreferenced (nothing depends on these, safe to change or remove): {}\n",
            orphans.join(", ")
        ));
    }
    out
}

/// Render ranked definitions, grouped by file and ordered by importance.
///
/// `line_cache` is passed in because the budget search calls this repeatedly;
/// the previous implementation re-read every file from disk on every iteration.
pub fn render_symbols(
    ranked_tags: &[RankedTag],
    metrics_by_file: &BTreeMap<String, FileMetrics>,
    line_cache: &BTreeMap<String, Vec<String>>,
) -> String {
    if ranked_tags.is_empty() {
        return String::new();
    }

    let mut file_tags: BTreeMap<String, Vec<&RankedTag>> = BTreeMap::new();
    for rt in ranked_tags {
        file_tags.entry(rt.tag.rel_fname.clone()).or_default().push(rt);
    }

    let mut sorted_files: Vec<(String, Vec<&RankedTag>)> = file_tags.into_iter().collect();
    sorted_files.sort_by(|a, b| {
        let max_a = a.1.iter().map(|rt| rt.rank).fold(0.0_f64, f64::max);
        let max_b = b.1.iter().map(|rt| rt.rank).fold(0.0_f64, f64::max);
        max_b
            .partial_cmp(&max_a)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut out = String::from("\n## SYMBOLS\n");

    for (rel_fname, tag_list) in &sorted_files {
        let header = match metrics_by_file.get(rel_fname) {
            Some(m) => format!(
                "\n{} (rank {:.4}, Ca {}, Ce {}, I {:.2}, risk {})\n",
                rel_fname, m.rank, m.afferent, m.efferent, m.instability, m.risk()
            ),
            None => format!("\n{rel_fname}\n"),
        };
        out.push_str(&header);

        let Some(lines) = line_cache.get(rel_fname) else {
            continue;
        };

        // Symbols are ranked, but printing them out of order would be unreadable;
        // sort by line for display and keep the rank in the file header.
        let mut lois: Vec<usize> = tag_list.iter().map(|rt| rt.tag.line).collect();
        lois.sort_unstable();
        lois.dedup();

        for loi in lois {
            if loi >= 1 && loi <= lines.len() {
                out.push_str(&format!("{:>5}: {}\n", loi, lines[loi - 1]));
            }
        }
    }

    out
}

pub fn build_line_cache(root: &Path, rel_fnames: &BTreeSet<String>) -> BTreeMap<String, Vec<String>> {
    rel_fnames
        .iter()
        .map(|rel| {
            let contents = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
            (rel.clone(), contents.lines().map(ToString::to_string).collect())
        })
        .collect()
}

pub fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let len = text.len();
    if len < 200 {
        return estimate_tokens(text);
    }

    let lines: Vec<&str> = text.lines().collect();
    let num_lines = lines.len();

    let step = (num_lines / 100).max(1);
    let sampled: Vec<&str> = lines.iter().step_by(step).copied().collect();
    let sample_text = sampled.join("\n");

    if sample_text.is_empty() {
        return estimate_tokens(text);
    }

    let sample_tokens = estimate_tokens(&sample_text);
    ((sample_tokens as f64 / sample_text.len() as f64) * len as f64) as usize
}

fn estimate_tokens(text: &str) -> usize {
    use std::sync::OnceLock;
    static TOKENIZER: OnceLock<Option<tiktoken_rs::CoreBPE>> = OnceLock::new();

    let tokenizer = TOKENIZER.get_or_init(|| {
        tiktoken_rs::get_bpe_from_tokenizer(tiktoken_rs::tokenizer::Tokenizer::O200kBase)
            .or_else(|_| {
                tiktoken_rs::get_bpe_from_tokenizer(tiktoken_rs::tokenizer::Tokenizer::Cl100kBase)
            })
            .ok()
    });

    if let Some(bpe) = tokenizer {
        return bpe.encode_with_special_tokens(text).len();
    }

    (text.len() / 4).max(1)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::repomap::graph::{DependencyEdge, FileMetrics};
    use crate::repomap::manifest::Manifest;
    use crate::repomap::tags::{Tag, TagKind};

    fn metric(name: &str, rank: f64, ca: usize, ce: usize) -> FileMetrics {
        let instability = if ca + ce == 0 { 1.0 } else { ce as f64 / (ca + ce) as f64 };
        FileMetrics {
            rel_fname: name.into(),
            rank,
            afferent: ca,
            efferent: ce,
            instability,
            def_count: 1,
        }
    }

    #[test]
    fn metrics_table_reports_risk_and_instability() {
        let out = render_metrics(&[metric("error.rs", 0.2, 16, 0)], 10);
        assert!(out.contains("error.rs"), "{out}");
        assert!(out.contains("high"), "{out}");
        assert!(out.contains("0.00"), "{out}");
    }

    #[test]
    fn dependency_lines_name_the_identifiers() {
        let edges = vec![DependencyEdge {
            from: "slop.rs".into(),
            to: "pathing.rs".into(),
            weight: 12.0,
            idents: vec!["normalize_path".into(), "expand_tilde".into()],
        }];
        let out = render_dependencies(&edges, 10);
        assert!(out.contains("slop.rs -> pathing.rs"), "{out}");
        assert!(out.contains("normalize_path"), "{out}");
    }

    #[test]
    fn structure_summarises_large_cycles_rather_than_listing_them() {
        let big: Vec<String> = (0..30).map(|i| format!("m{i}.rs")).collect();
        let out = render_structure(&[big], &[], &[]);
        assert!(out.contains("30 files mutually dependent"), "{out}");
        assert!(!out.contains("m29.rs"), "{out}");
    }

    #[test]
    fn structure_lists_small_cycles_in_full() {
        let out = render_structure(&[vec!["a.rs".into(), "b.rs".into()]], &[], &[]);
        assert!(out.contains("a.rs <-> b.rs"), "{out}");
    }

    #[test]
    fn structure_reports_chokepoints_and_orphans() {
        let out = render_structure(&[], &["hub.rs".into()], &["dead.rs".into()]);
        assert!(out.contains("chokepoints"), "{out}");
        assert!(out.contains("hub.rs"), "{out}");
        assert!(out.contains("safe to change or remove"), "{out}");
        assert!(out.contains("dead.rs"), "{out}");
    }

    #[test]
    fn symbol_section_carries_risk_context_in_the_file_header() {
        let ranked = vec![RankedTag {
            rank: 0.5,
            tag: Tag {
                rel_fname: "core.rs".into(),
                line: 1,
                name: "run".into(),
                kind: TagKind::Def,
            },
        }];
        let mut metrics = BTreeMap::new();
        metrics.insert("core.rs".to_string(), metric("core.rs", 0.5, 9, 1));
        let mut cache = BTreeMap::new();
        cache.insert("core.rs".to_string(), vec!["pub fn run() {}".to_string()]);

        let out = render_symbols(&ranked, &metrics, &cache);
        assert!(out.contains("core.rs (rank 0.5000, Ca 9, Ce 1"), "{out}");
        assert!(out.contains("risk high"), "{out}");
        assert!(out.contains("    1: pub fn run() {}"), "{out}");
    }

    #[test]
    fn manifest_section_states_its_source_and_totals() {
        let m = Manifest {
            files: vec!["Cargo.toml".into(), "src/main.rs".into()],
            truncated: 3,
            from_git: true,
        };
        let out = render_manifest(&m, &BTreeSet::new());
        assert!(out.contains("2 of 5 files"), "{out}");
        assert!(out.contains("git ls-files"), "{out}");
        assert!(out.contains("Cargo.toml"), "{out}");
    }

    #[test]
    fn empty_inputs_render_nothing() {
        assert!(render_metrics(&[], 10).is_empty());
        assert!(render_dependencies(&[], 10).is_empty());
        assert!(render_structure(&[], &[], &[]).is_empty());
    }

    #[test]
    fn token_counting_scales_with_length() {
        let short = "hello world";
        let long = "hello world\n".repeat(500);
        assert!(count_tokens(&long) > count_tokens(short));
        assert_eq!(count_tokens(""), 0);
    }
}
