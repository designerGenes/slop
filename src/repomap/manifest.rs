use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use super::importance;

/// The complete file inventory for a repository.
///
/// `discover_source_files` only ever saw the ~20 extensions the tag extractor
/// understands, so `Cargo.toml`, `README.md`, `.github/workflows/*`, migrations
/// and fixtures were invisible to a remote agent. This walks everything git
/// tracks instead, which is both broader and already gitignore-correct.
pub struct Manifest {
    pub files: Vec<String>,
    pub truncated: usize,
    /// True when the listing came from git rather than a filesystem walk.
    pub from_git: bool,
}

/// Directories whose contents are never interesting to an agent.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "__pycache__",
    "venv",
    "env",
    "target",
    "build",
    "dist",
    ".repomap.tags.cache.v1",
    ".slop",
    "vendor",
    "Pods",
    ".next",
    ".venv",
];

/// Extensions the tag extractor can parse. Used to decide which files are
/// candidates for symbol extraction, never to decide what appears in the
/// manifest.
pub const PARSEABLE_EXTS: &[&str] = &[
    "rs", "py", "js", "jsx", "mjs", "cjs", "ts", "tsx", "go", "c", "h", "cpp", "cc", "cxx", "hpp",
    "hxx", "java", "rb", "gd", "swift",
];

pub fn is_parseable(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| PARSEABLE_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// List every file in the repository, preferring git's own index.
///
/// `git ls-files` gives exactly "every file referenced by anything in the
/// containing git repo" without reimplementing gitignore semantics, and it
/// includes tracked-but-unparseable files the old walker dropped on the floor.
pub fn collect_repo_files(root: &Path, max_files: usize) -> Manifest {
    let (mut files, from_git) = match git_ls_files(root) {
        Some(files) if !files.is_empty() => (files, true),
        _ => (walk_all_files(root), false),
    };

    files.sort();
    files.dedup();

    let total = files.len();
    let truncated = total.saturating_sub(max_files);
    if truncated > 0 {
        // Keep the files an agent is most likely to need to ask for: anything
        // recognised as structurally important, then parseable source, then the
        // rest in path order.
        let mut ranked: Vec<(u8, String)> = files
            .into_iter()
            .map(|f| {
                let tier = if importance::is_important(&f) {
                    0
                } else if is_parseable(&f) {
                    1
                } else {
                    2
                };
                (tier, f)
            })
            .collect();
        ranked.sort();
        files = ranked
            .into_iter()
            .take(max_files)
            .map(|(_, f)| f)
            .collect();
        files.sort();
    }

    Manifest {
        files,
        truncated,
        from_git,
    }
}

fn git_ls_files(root: &Path) -> Option<Vec<String>> {
    let output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .filter(|line| {
                !Path::new(line)
                    .components()
                    .any(|c| SKIP_DIRS.contains(&c.as_os_str().to_string_lossy().as_ref()))
            })
            .map(ToString::to_string)
            .collect(),
    )
}

fn walk_all_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    walk(root, root, &mut files);
    files
}

fn walk(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy().to_string();

        if path.is_dir() {
            if SKIP_DIRS.contains(&name_str.as_str()) {
                continue;
            }
            // Hidden directories are skipped except the ones that carry real
            // project configuration.
            if name_str.starts_with('.') && name_str != ".github" && name_str != ".cargo" {
                continue;
            }
            walk(root, &path, files);
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                files.push(rel.to_string_lossy().to_string());
            }
        }
    }
}

/// Render the inventory as an indented directory tree.
///
/// Files carrying extra signal are annotated: `*` for structurally important
/// files (build manifests, CI, READMEs) and `~` for files whose symbols were
/// parsed into the dependency graph.
pub fn render_tree(manifest: &Manifest, in_bundle: &BTreeSet<String>) -> String {
    let mut dirs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for file in &manifest.files {
        let path = Path::new(file);
        let parent = path
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| file.clone());
        dirs.entry(parent).or_default().push(name);
    }

    let mut out = String::new();
    for (dir, names) in &dirs {
        let depth = if dir.is_empty() {
            0
        } else {
            dir.matches('/').count() + 1
        };
        let label = if dir.is_empty() { "." } else { dir.as_str() };
        out.push_str(&format!("{}{}/\n", "  ".repeat(depth.saturating_sub(1)), label));

        for name in names {
            let full = if dir.is_empty() {
                name.clone()
            } else {
                format!("{dir}/{name}")
            };
            let mut marks = String::new();
            if importance::is_important(&full) {
                marks.push('*');
            }
            if is_parseable(&full) {
                marks.push('~');
            }
            if in_bundle.contains(&full) {
                marks.push('@');
            }
            let suffix = if marks.is_empty() {
                String::new()
            } else {
                format!(" {marks}")
            };
            out.push_str(&format!("{}{}{}\n", "  ".repeat(depth), name, suffix));
        }
    }

    if manifest.truncated > 0 {
        out.push_str(&format!(
            "... and {} more files omitted for space\n",
            manifest.truncated
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(files: &[&str]) -> Manifest {
        Manifest {
            files: files.iter().map(|s| s.to_string()).collect(),
            truncated: 0,
            from_git: true,
        }
    }

    #[test]
    fn manifest_keeps_non_source_files() {
        // The whole point: these are what the old walker discarded.
        let m = manifest(&["Cargo.toml", "README.md", "src/main.rs"]);
        let tree = render_tree(&m, &BTreeSet::new());
        assert!(tree.contains("Cargo.toml"), "{tree}");
        assert!(tree.contains("README.md"), "{tree}");
        assert!(tree.contains("main.rs"), "{tree}");
    }

    #[test]
    fn annotates_important_parseable_and_bundled_files() {
        let m = manifest(&["Cargo.toml", "src/main.rs", "notes.txt"]);
        let mut bundle = BTreeSet::new();
        bundle.insert("src/main.rs".to_string());
        let tree = render_tree(&m, &bundle);

        assert!(tree.contains("Cargo.toml *"), "{tree}");
        assert!(tree.contains("main.rs ~@"), "{tree}");
        assert!(tree.contains("notes.txt\n"), "{tree}");
    }

    #[test]
    fn nests_directories_by_depth() {
        let m = manifest(&["src/repomap/graph.rs", "src/main.rs"]);
        let tree = render_tree(&m, &BTreeSet::new());
        let src = tree.find("src/").expect("src dir");
        let nested = tree.find("src/repomap/").expect("nested dir");
        assert!(src < nested, "{tree}");
    }

    #[test]
    fn is_parseable_matches_only_supported_extensions() {
        assert!(is_parseable("src/main.rs"));
        assert!(is_parseable("App.SWIFT"));
        assert!(!is_parseable("Cargo.toml"));
        assert!(!is_parseable("README"));
    }

    #[test]
    fn truncation_prefers_important_then_source_then_rest() {
        let mut files: Vec<String> = (0..50).map(|i| format!("assets/img{i}.png")).collect();
        files.push("Cargo.toml".into());
        files.push("src/main.rs".into());
        let m = Manifest { files, truncated: 0, from_git: true };

        let mut ranked: Vec<(u8, String)> = m
            .files
            .into_iter()
            .map(|f| {
                let tier = if importance::is_important(&f) { 0 }
                           else if is_parseable(&f) { 1 } else { 2 };
                (tier, f)
            })
            .collect();
        ranked.sort();
        let kept: Vec<String> = ranked.into_iter().take(2).map(|(_, f)| f).collect();
        assert_eq!(kept, vec!["Cargo.toml".to_string(), "src/main.rs".to_string()]);
    }

    #[test]
    fn reports_truncation_in_the_rendered_tree() {
        let m = Manifest {
            files: vec!["src/main.rs".into()],
            truncated: 7,
            from_git: true,
        };
        assert!(render_tree(&m, &BTreeSet::new()).contains("7 more files omitted"));
    }
}
