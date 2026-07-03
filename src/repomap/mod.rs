pub mod graph;
pub mod importance;
pub mod render;
pub mod tags;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use graph::RankedTag;
use tags::Tag;

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

    pub fn get_repo_map(
        &self,
        chat_files: &[String],
        other_files: &[String],
    ) -> Option<String> {
        if self.map_tokens == 0 || other_files.is_empty() {
            return None;
        }

        let chat_rel_fnames: HashSet<String> = chat_files
            .iter()
            .filter_map(|f| self.rel_fname(f))
            .collect();

        let all_files: Vec<String> = chat_files
            .iter()
            .chain(other_files.iter())
            .cloned()
            .collect();

        let mut all_tags: Vec<Tag> = Vec::new();

        for fname in &all_files {
            if !Path::new(fname).exists() {
                continue;
            }
            let rel = self.rel_fname(fname).unwrap_or_else(|| fname.clone());
            let tags = tags::extract_tags(fname, &rel);
            all_tags.extend(tags);
        }

        if all_tags.is_empty() {
            return None;
        }

        let mentioned_idents = HashSet::new();
        let mentioned_fnames = HashSet::new();

        let ranked = graph::build_and_rank(
            &all_tags,
            &chat_rel_fnames,
            &mentioned_idents,
            &mentioned_fnames,
        );

        if ranked.is_empty() {
            return None;
        }

        let max_tokens = if chat_files.is_empty() {
            self.map_tokens * 8
        } else {
            self.map_tokens
        };

        let tree_output = self.binary_search_tokens(&ranked, max_tokens);

        tree_output
    }

    fn binary_search_tokens(
        &self,
        ranked: &[RankedTag],
        max_tokens: usize,
    ) -> Option<String> {
        let mut left = 0usize;
        let mut right = ranked.len();
        let mut best: Option<String> = None;

        while left <= right {
            let mid = (left + right) / 2;
            if mid == 0 {
                left = 1;
                continue;
            }

            let selected = &ranked[..mid];
            let tree = render::render_tree(selected, &self.root);

            if tree.is_empty() {
                right = mid.saturating_sub(1);
                continue;
            }

            let tokens = render::count_tokens(&tree);

            if tokens <= max_tokens {
                best = Some(tree);
                left = mid + 1;
            } else {
                right = mid.saturating_sub(1);
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

pub fn discover_source_files(root: &Path) -> Vec<String> {
    let skip_dirs: HashSet<&str> = HashSet::from([
        ".git", "node_modules", "__pycache__", "venv", "env",
        "target", "build", "dist", ".repomap.tags.cache.v1",
    ]);

    let supported_exts: HashSet<&str> = HashSet::from([
        "rs", "py", "js", "jsx", "mjs", "cjs", "ts", "tsx",
        "go", "c", "h", "cpp", "cc", "cxx", "hpp", "hxx",
        "java", "rb", "gd", "swift",
    ]);

    let mut files = Vec::new();

    fn walk(
        dir: &Path,
        skip_dirs: &HashSet<&str>,
        supported_exts: &HashSet<&str>,
        files: &mut Vec<String>,
    ) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            if name_str.starts_with('.') && name_str != ".env" && name_str != ".gitignore" {
                continue;
            }

            if path.is_dir() {
                if skip_dirs.contains(name_str.as_ref()) {
                    continue;
                }
                walk(&path, skip_dirs, supported_exts, files);
            } else if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if supported_exts.contains(ext) {
                        files.push(path.to_string_lossy().to_string());
                    }
                }
            }
        }
    }

    walk(root, &skip_dirs, &supported_exts, &mut files);

    files.sort();
    files
}
