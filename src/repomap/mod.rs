pub mod graph;
pub mod importance;
pub mod manifest;
pub mod render;
pub mod tags;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use graph::{FileMetrics, GraphAnalysis, RankedTag};
use tags::Tag;

/// Hard ceiling on manifest entries, so a monorepo cannot produce a megabyte
/// of tree output.
const MAX_MANIFEST_FILES: usize = 4000;
const MAX_METRICS_ROWS: usize = 60;
const MAX_DEPENDENCY_EDGES: usize = 80;

pub struct RepoMap {
    pub map_tokens: usize,
    pub root: PathBuf,
}

impl RepoMap {
    pub fn new(map_tokens: usize, root: &Path) -> Self {
        Self {
            map_tokens,
            root: root.to_path_buf(),
        }
    }

    pub fn get_repo_map(&self, chat_files: &[String], other_files: &[String]) -> Option<String> {
        if self.map_tokens == 0 {
            return None;
        }

        let chat_rel_fnames: HashSet<String> =
            chat_files.iter().filter_map(|f| self.rel_fname(f)).collect();

        // The manifest is unconditional. It is the only part of the map that
        // answers "what else exists in this repo", so it is built before the
        // token budget is spent on anything else and is never trimmed away by
        // the symbol search.
        let manifest = manifest::collect_repo_files(&self.root, MAX_MANIFEST_FILES);

        let mut parseable: BTreeSet<String> = manifest
            .files
            .iter()
            .filter(|f| manifest::is_parseable(f))
            .cloned()
            .collect();
        for fname in chat_files.iter().chain(other_files.iter()) {
            if let Some(rel) = self.rel_fname(fname) {
                if manifest::is_parseable(&rel) {
                    parseable.insert(rel);
                }
            }
        }

        let mut all_tags: Vec<Tag> = Vec::new();
        for rel in &parseable {
            let abs = self.root.join(rel);
            if !abs.exists() {
                continue;
            }
            all_tags.extend(tags::extract_tags(&abs.to_string_lossy(), rel));
        }

        let bundled: BTreeSet<String> = chat_rel_fnames.iter().cloned().collect();

        let mut sections = String::from(render::LEGEND);
        sections.push('\n');
        sections.push_str(&render::render_manifest(&manifest, &bundled));

        if all_tags.is_empty() {
            // No parseable source: the manifest alone is still worth shipping.
            return Some(sections);
        }

        let analysis = graph::analyze(
            &all_tags,
            &chat_rel_fnames,
            &HashSet::new(),
            &HashSet::new(),
        );

        sections.push_str(&render::render_metrics(&analysis.metrics, MAX_METRICS_ROWS));
        sections.push_str(&render::render_dependencies(
            &analysis.edges,
            MAX_DEPENDENCY_EDGES,
        ));
        sections.push_str(&render::render_structure(
            &analysis.cycles,
            &analysis.chokepoints,
            &analysis.orphans,
        ));

        // Files shipped verbatim in this slop are dropped from the symbol
        // listing. Previously they were boosted 20x and crowded out everything
        // else, so the map described what the agent could already read.
        let symbol_budget = self
            .map_tokens
            .saturating_sub(render::count_tokens(&sections));
        if symbol_budget > 0 {
            if let Some(symbols) = self.fit_symbols(&analysis, &bundled, symbol_budget) {
                sections.push_str(&symbols);
            }
        }

        Some(sections)
    }

    fn fit_symbols(
        &self,
        analysis: &GraphAnalysis,
        bundled: &BTreeSet<String>,
        budget: usize,
    ) -> Option<String> {
        let owned: Vec<RankedTag> = analysis
            .ranked_tags
            .iter()
            .filter(|rt| !bundled.contains(&rt.tag.rel_fname))
            .map(|rt| RankedTag {
                rank: rt.rank,
                tag: rt.tag.clone(),
            })
            .collect();

        if owned.is_empty() {
            return None;
        }

        let metrics_by_file: BTreeMap<String, FileMetrics> = analysis
            .metrics
            .iter()
            .map(|m| (m.rel_fname.clone(), m.clone()))
            .collect();

        let rel_fnames: BTreeSet<String> =
            owned.iter().map(|rt| rt.tag.rel_fname.clone()).collect();
        // Read each file once, not once per binary-search probe.
        let line_cache = render::build_line_cache(&self.root, &rel_fnames);

        let mut left = 1usize;
        let mut right = owned.len();
        let mut best: Option<String> = None;

        while left <= right {
            let mid = left + (right - left) / 2;
            let rendered = render::render_symbols(&owned[..mid], &metrics_by_file, &line_cache);
            if rendered.is_empty() {
                if mid == 1 {
                    break;
                }
                right = mid - 1;
                continue;
            }
            if render::count_tokens(&rendered) <= budget {
                best = Some(rendered);
                left = mid + 1;
            } else {
                if mid == 1 {
                    break;
                }
                right = mid - 1;
            }
        }

        best
    }

    fn rel_fname(&self, fname: &str) -> Option<String> {
        let path = Path::new(fname);
        path.strip_prefix(&self.root)
            .ok()
            .map(|p| p.to_string_lossy().to_string())
    }
}

pub fn generate_repomap(
    repo_root: &Path,
    seed_files: &[PathBuf],
    map_tokens: usize,
) -> Option<String> {
    let repo_map = RepoMap::new(map_tokens, repo_root);
    let chat_files: Vec<String> = seed_files
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    let other_files = discover_source_files(repo_root);
    repo_map.get_repo_map(&chat_files, &other_files)
}

/// Retained for callers that want only the parseable subset. The manifest no
/// longer depends on this; it walks everything git tracks.
pub fn discover_source_files(root: &Path) -> Vec<String> {
    let m = manifest::collect_repo_files(root, MAX_MANIFEST_FILES);
    let mut files: Vec<String> = m
        .files
        .into_iter()
        .filter(|f| manifest::is_parseable(f))
        .map(|f| root.join(f).to_string_lossy().to_string())
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, body).expect("write");
    }

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(root, "Cargo.toml", "[package]\nname = \"x\"\n");
        write(root, "README.md", "# x\n");
        write(root, "assets/logo.png", "notreallyapng");
        write(root, "src/main.rs", "fn main() { run(); }\n");
        write(root, "src/core.rs", "pub fn run() {}\n");
        dir
    }

    #[test]
    fn manifest_includes_files_the_old_walker_dropped() {
        let f = fixture();
        let map = RepoMap::new(4096, f.path())
            .get_repo_map(&[], &[])
            .expect("map");
        // These have no parseable extension and were previously invisible.
        assert!(map.contains("Cargo.toml"), "{map}");
        assert!(map.contains("README.md"), "{map}");
        assert!(map.contains("logo.png"), "{map}");
    }

    #[test]
    fn manifest_survives_a_tiny_token_budget() {
        let f = fixture();
        // One token of budget: the symbol section must be sacrificed, never the
        // inventory of what exists.
        let map = RepoMap::new(1, f.path()).get_repo_map(&[], &[]).expect("map");
        assert!(map.contains("## MANIFEST"), "{map}");
        assert!(map.contains("Cargo.toml"), "{map}");
    }

    #[test]
    fn map_is_produced_even_with_no_other_files() {
        // Previously `other_files.is_empty()` short-circuited to None, so a
        // single-file slop produced no graph at all.
        let f = fixture();
        let map = RepoMap::new(4096, f.path())
            .get_repo_map(&[f.path().join("src/main.rs").to_string_lossy().to_string()], &[])
            .expect("map");
        assert!(map.contains("## MANIFEST"), "{map}");
    }

    #[test]
    fn bundled_files_are_marked_and_excluded_from_symbols() {
        let f = fixture();
        let seed = f.path().join("src/main.rs").to_string_lossy().to_string();
        let map = RepoMap::new(4096, f.path())
            .get_repo_map(&[seed], &[])
            .expect("map");

        // Marked in the manifest ...
        assert!(map.contains("main.rs ~@"), "{map}");
        // ... but not re-listed under SYMBOLS, where budget is scarce.
        if let Some(symbols) = map.split("## SYMBOLS").nth(1) {
            assert!(!symbols.contains("src/main.rs ("), "{symbols}");
        }
    }

    #[test]
    fn legend_explains_the_columns_and_the_request_convention() {
        let f = fixture();
        let map = RepoMap::new(4096, f.path())
            .get_repo_map(&[], &[])
            .expect("map");
        assert!(map.contains("Ca = files depending on this one"), "{map}");
        assert!(map.contains("#SLOP_REQUEST"), "{map}");
    }

    #[test]
    fn zero_token_budget_disables_the_map() {
        let f = fixture();
        assert!(RepoMap::new(0, f.path()).get_repo_map(&[], &[]).is_none());
    }
}
