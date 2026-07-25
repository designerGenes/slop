//! Human-facing tree rendering for the slopify run report.
//!
//! Deliberately dependency-free: slop has no colour crate, and the two escape
//! sequences needed here do not justify adding one. Colour is opt-out via
//! `NO_COLOR`, opt-in via `CLICOLOR_FORCE`, and otherwise gated on stderr
//! being a terminal, so piped output stays plain.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use crate::models::IgnoredEntry;

/// A light, pleasant green (xterm 114).
const GREEN: &str = "\x1b[38;5;114m";
/// An assertive red (xterm 196).
const RED: &str = "\x1b[38;5;196m";
const RESET: &str = "\x1b[0m";

const BRANCH: &str = "├── ";
const LAST_BRANCH: &str = "└── ";
const VERTICAL: &str = "│   ";
const BLANK: &str = "    ";

/// Whether ANSI colour should be emitted on stderr.
pub fn colors_enabled() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    if std::env::var_os("CLICOLOR_FORCE").is_some_and(|value| value != "0") {
        return true;
    }
    std::io::stderr().is_terminal()
}

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    is_dir: bool,
    ignored: bool,
}

impl Node {
    fn insert(&mut self, components: &[String], leaf_is_dir: bool, ignored: bool) {
        let Some((head, rest)) = components.split_first() else {
            return;
        };

        let child = self.children.entry(head.clone()).or_default();
        if rest.is_empty() {
            child.is_dir = leaf_is_dir;
            child.ignored = ignored;
        } else {
            child.is_dir = true;
            child.insert(rest, leaf_is_dir, ignored);
        }
    }
}

/// Render the set of files being slopped as a tree rooted at `root`.
///
/// When `verbose` is set, entries excluded by `.slopignore` are interleaved
/// in place and tagged `[IGNORED]`. An ignored *directory* appears as a single
/// pruned node — slop never descended into it, so it has nothing to show.
pub fn render_walk_tree(
    root: &Path,
    included: &[PathBuf],
    ignored: &[IgnoredEntry],
    verbose: bool,
    color: bool,
) -> String {
    let mut tree = Node {
        is_dir: true,
        ..Node::default()
    };

    for path in included {
        if let Some(components) = relative_components(root, path) {
            tree.insert(&components, false, false);
        }
    }

    if verbose {
        for entry in ignored {
            if let Some(components) = relative_components(root, &entry.path) {
                tree.insert(&components, entry.is_dir, true);
            }
        }
    }

    let mut out = String::new();
    out.push_str(&root_label(root));
    out.push('\n');
    render_children(&tree, "", &mut out, color);
    out
}

fn render_children(node: &Node, prefix: &str, out: &mut String, color: bool) {
    // Files first, then directories — each alphabetical, courtesy of BTreeMap.
    let ordered: Vec<(&String, &Node)> = node
        .children
        .iter()
        .filter(|(_, child)| !child.is_dir)
        .chain(node.children.iter().filter(|(_, child)| child.is_dir))
        .collect();

    let last_index = ordered.len().saturating_sub(1);
    for (index, (name, child)) in ordered.iter().enumerate() {
        let is_last = index == last_index;
        let connector = if is_last { LAST_BRANCH } else { BRANCH };
        let suffix = if child.ignored { " [IGNORED]" } else { "" };
        let line = format!("{prefix}{connector}{name}{suffix}");

        out.push_str(&paint(&line, child.ignored, color));
        out.push('\n');

        if !child.children.is_empty() {
            let next_prefix = format!("{prefix}{}", if is_last { BLANK } else { VERTICAL });
            render_children(child, &next_prefix, out, color);
        }
    }
}

fn paint(line: &str, ignored: bool, color: bool) -> String {
    if !color {
        return line.to_string();
    }
    let code = if ignored { RED } else { GREEN };
    format!("{code}{line}{RESET}")
}

fn root_label(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| root.display().to_string())
}

fn relative_components(root: &Path, path: &Path) -> Option<Vec<String>> {
    let relative = path.strip_prefix(root).ok()?;
    let components: Vec<String> = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect();

    if components.is_empty() {
        None
    } else {
        Some(components)
    }
}

/// Deepest directory containing every path in `paths`.
pub fn common_root(paths: &[PathBuf]) -> PathBuf {
    let mut iter = paths.iter();
    let Some(first) = iter.next() else {
        return PathBuf::from(".");
    };

    let mut common = first.clone();
    for path in iter {
        common = common_ancestor(&common, path);
    }

    // A single file (or several copies of one path) collapses to the file
    // itself; back up one level so it has somewhere to hang.
    if paths.iter().any(|path| path == &common) {
        if let Some(parent) = common.parent() {
            return parent.to_path_buf();
        }
    }

    common
}

fn common_ancestor(left: &Path, right: &Path) -> PathBuf {
    let left_components: Vec<_> = left.components().collect();
    let right_components: Vec<_> = right.components().collect();

    let mut result = PathBuf::new();
    for index in 0..left_components.len().min(right_components.len()) {
        if left_components[index] != right_components[index] {
            break;
        }
        result.push(left_components[index]);
    }

    if result.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        result
    }
}

/// Human-readable byte count, in kb as the run report advertises.
pub fn format_size(bytes: u64) -> String {
    format!("{:.1} kb", bytes as f64 / 1024.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{IgnoreReason, IgnoredEntry};

    fn ignored_file(path: &str) -> IgnoredEntry {
        IgnoredEntry {
            path: PathBuf::from(path),
            is_dir: false,
            reason: IgnoreReason::SlopIgnore,
        }
    }

    #[test]
    fn renders_nested_tree_with_files_before_directories() {
        let root = PathBuf::from("/tmp/project_root");
        let included = vec![
            PathBuf::from("/tmp/project_root/dir1/file1.txt"),
            PathBuf::from("/tmp/project_root/dir1/dir2/file2.txt"),
            PathBuf::from("/tmp/project_root/dir1/dir2/dir3/file4"),
            PathBuf::from("/tmp/project_root/dir1/dir2/dir3/file5.js"),
        ];

        let rendered = render_walk_tree(&root, &included, &[], false, false);
        let expected = "project_root\n\
                        └── dir1\n    \
                        ├── file1.txt\n    \
                        └── dir2\n        \
                        ├── file2.txt\n        \
                        └── dir3\n            \
                        ├── file4\n            \
                        └── file5.js\n";

        assert_eq!(rendered, expected);
    }

    #[test]
    fn hides_ignored_entries_unless_verbose() {
        let root = PathBuf::from("/tmp/project_root");
        let included = vec![PathBuf::from("/tmp/project_root/keep.txt")];
        let ignored = vec![ignored_file("/tmp/project_root/skip.md")];

        let quiet = render_walk_tree(&root, &included, &ignored, false, false);
        assert!(!quiet.contains("skip.md"));
        assert!(!quiet.contains("[IGNORED]"));

        let loud = render_walk_tree(&root, &included, &ignored, true, false);
        assert!(loud.contains("skip.md [IGNORED]"));
    }

    #[test]
    fn ignored_directory_renders_as_a_pruned_leaf() {
        let root = PathBuf::from("/tmp/project_root");
        let included = vec![PathBuf::from("/tmp/project_root/keep.txt")];
        let ignored = vec![IgnoredEntry {
            path: PathBuf::from("/tmp/project_root/vendor"),
            is_dir: true,
            reason: IgnoreReason::SlopIgnore,
        }];

        let rendered = render_walk_tree(&root, &included, &ignored, true, false);
        assert!(rendered.contains("vendor [IGNORED]"));
        // A pruned directory is a leaf: nothing beneath it was ever walked.
        assert_eq!(rendered.lines().count(), 3);
    }

    #[test]
    fn colors_ignored_lines_red_and_the_rest_green() {
        let root = PathBuf::from("/tmp/project_root");
        let included = vec![PathBuf::from("/tmp/project_root/keep.txt")];
        let ignored = vec![ignored_file("/tmp/project_root/skip.md")];

        // Both are files, so they sort alphabetically: keep.txt, then skip.md.
        let rendered = render_walk_tree(&root, &included, &ignored, true, true);
        assert!(rendered.contains(&format!("{GREEN}├── keep.txt{RESET}")));
        assert!(rendered.contains(&format!("{RED}└── skip.md [IGNORED]{RESET}")));
        assert!(
            !rendered.starts_with(GREEN),
            "the root label itself is left unpainted"
        );
    }

    #[test]
    fn plain_rendering_contains_no_escape_sequences() {
        let root = PathBuf::from("/tmp/project_root");
        let included = vec![PathBuf::from("/tmp/project_root/keep.txt")];
        let rendered = render_walk_tree(&root, &included, &[], false, false);
        assert!(!rendered.contains('\x1b'));
    }

    #[test]
    fn common_root_of_several_files_is_their_shared_directory() {
        let paths = vec![
            PathBuf::from("/a/b/c/one.txt"),
            PathBuf::from("/a/b/d/two.txt"),
        ];
        assert_eq!(common_root(&paths), PathBuf::from("/a/b"));
    }

    #[test]
    fn common_root_of_a_single_file_is_its_parent() {
        let paths = vec![PathBuf::from("/a/b/only.txt")];
        assert_eq!(common_root(&paths), PathBuf::from("/a/b"));
    }

    #[test]
    fn formats_size_in_kilobytes() {
        assert_eq!(format_size(1024), "1.0 kb");
        assert_eq!(format_size(1536), "1.5 kb");
        assert_eq!(format_size(0), "0.0 kb");
    }
}
