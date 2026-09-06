use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::repomap::tags::{self, TagKind};

pub struct FileAdjacency {
    pub outgoing: HashMap<String, HashSet<String>>,
    pub incoming: HashMap<String, HashSet<String>>,
}

impl FileAdjacency {
    pub fn neighbors(&self, rel_path: &str) -> HashSet<String> {
        let mut result = HashSet::new();
        if let Some(outs) = self.outgoing.get(rel_path) {
            result.extend(outs.iter().cloned());
        }
        if let Some(ins) = self.incoming.get(rel_path) {
            result.extend(ins.iter().cloned());
        }
        result
    }
}

pub fn build_adjacency(corpus_root: &Path) -> FileAdjacency {
    let files = collect_repo_files(corpus_root);
    let mut defines: HashMap<String, HashSet<String>> = HashMap::new();
    let mut references: HashMap<String, HashSet<String>> = HashMap::new();
    let mut all_rel_fnames: HashSet<String> = HashSet::new();

    for file_path in &files {
        let rel = file_path
            .strip_prefix(corpus_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();
        let abs = file_path.to_string_lossy().to_string();
        all_rel_fnames.insert(rel.clone());

        let tags = tags::extract_tags(&abs, &rel);
        for tag in &tags {
            match tag.kind {
                TagKind::Def => {
                    defines
                        .entry(tag.name.clone())
                        .or_default()
                        .insert(rel.clone());
                }
                TagKind::Ref => {
                    references
                        .entry(tag.name.clone())
                        .or_default()
                        .insert(rel.clone());
                }
            }
        }
    }

    let mut outgoing: HashMap<String, HashSet<String>> = HashMap::new();
    let mut incoming: HashMap<String, HashSet<String>> = HashMap::new();

    for (name, ref_fnames) in &references {
        if let Some(defs) = defines.get(name) {
            for ref_fname in ref_fnames {
                for def_fname in defs {
                    if ref_fname != def_fname {
                        outgoing
                            .entry(ref_fname.clone())
                            .or_default()
                            .insert(def_fname.clone());
                        incoming
                            .entry(def_fname.clone())
                            .or_default()
                            .insert(ref_fname.clone());
                    }
                }
            }
        }
    }

    for rel in &all_rel_fnames {
        outgoing.entry(rel.clone()).or_default();
        incoming.entry(rel.clone()).or_default();
    }

    FileAdjacency { outgoing, incoming }
}

pub fn bfs_neighbors(
    adjacency: &FileAdjacency,
    seeds: &[PathBuf],
    corpus_root: &Path,
    hops: usize,
) -> Vec<(String, usize)> {
    let mut visited: HashMap<String, usize> = HashMap::new();
    let mut queue: Vec<(String, usize)> = Vec::new();

    for seed in seeds {
        let rel = seed
            .strip_prefix(corpus_root)
            .unwrap_or(seed)
            .to_string_lossy()
            .to_string();
        if visited.insert(rel.clone(), 0).is_none() {
            queue.push((rel, 0));
        }
    }

    while let Some((node, depth)) = queue.first().cloned() {
        queue.remove(0);
        if depth >= hops {
            continue;
        }
        for neighbor in adjacency.neighbors(&node) {
            if !visited.contains_key(&neighbor) {
                visited.insert(neighbor.clone(), depth + 1);
                queue.push((neighbor, depth + 1));
            }
        }
    }

    visited.into_iter().collect()
}

pub fn resolve_symbol(corpus_root: &Path, symbol: &str) -> Vec<String> {
    let files = collect_repo_files(corpus_root);
    let mut defines: HashMap<String, HashSet<String>> = HashMap::new();
    let mut references: HashMap<String, HashSet<String>> = HashMap::new();
    let mut file_defs: HashMap<String, HashSet<String>> = HashMap::new();
    let mut file_refs: HashMap<String, HashSet<String>> = HashMap::new();

    for file_path in &files {
        let rel = file_path
            .strip_prefix(corpus_root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();
        let abs = file_path.to_string_lossy().to_string();
        let tags = tags::extract_tags(&abs, &rel);

        for tag in &tags {
            match tag.kind {
                TagKind::Def => {
                    defines
                        .entry(tag.name.clone())
                        .or_default()
                        .insert(rel.clone());
                    file_defs
                        .entry(rel.clone())
                        .or_default()
                        .insert(tag.name.clone());
                }
                TagKind::Ref => {
                    references
                        .entry(tag.name.clone())
                        .or_default()
                        .insert(rel.clone());
                    file_refs
                        .entry(rel.clone())
                        .or_default()
                        .insert(tag.name.clone());
                }
            }
        }
    }

    let def_files: HashSet<String> = defines.get(symbol).cloned().unwrap_or_default();
    let callers: HashSet<String> = references.get(symbol).cloned().unwrap_or_default();

    let mut callees: HashSet<String> = HashSet::new();
    for def_file in &def_files {
        if let Some(refs) = file_refs.get(def_file) {
            for ref_name in refs {
                if let Some(defs) = defines.get(ref_name) {
                    callees.extend(defs.iter().cloned());
                }
            }
        }
    }

    def_files
        .iter()
        .chain(callers.iter())
        .chain(callees.iter())
        .cloned()
        .collect()
}

fn collect_repo_files(corpus_root: &Path) -> Vec<PathBuf> {
    crate::repomap::discover_source_files(corpus_root)
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

pub fn rel_to_abs(corpus_root: &Path, rel: &str) -> PathBuf {
    corpus_root.join(rel)
}
