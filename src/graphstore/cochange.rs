//! Co-change signal mined from git history.
//!
//! The symbol graph only sees coupling the parser can prove: an identifier
//! defined here and referenced there. Real codebases are full of coupling no
//! parser will ever see — a struct and the migration that shapes it, a
//! component and its snapshot fixture, a feature flag and the three call sites
//! that read it by string. Files that keep changing in the same commit are
//! telling you they belong to the same unit of work, and that is exactly the
//! question a context page needs answered.
//!
//! Two guards keep the signal honest. Commits touching more than
//! `max_files_per_commit` files are dropped outright: a formatter sweep, a
//! license header change or a mass rename would otherwise connect everything to
//! everything. And each surviving commit's contribution is divided by its own
//! breadth, so a focused two-file commit says more about coupling than a
//! twenty-file one.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use super::model::CoChangeEdge;

/// Marker prefixed to commit lines so a file named like a hash cannot be
/// mistaken for one. A NUL byte cannot appear in a git path.
///
/// The pretty format below spells it `%x00`, which is expanded by git into the
/// output stream. Writing a literal NUL into the argument instead would fail at
/// spawn time: a process argument is a C string and cannot contain one.
const COMMIT_MARKER: char = '\0';
const COMMIT_FORMAT: &str = "--pretty=format:%x00%H";

pub struct CoChangeOptions {
    /// How far back to read. History has diminishing returns and old commits
    /// describe a codebase that no longer exists.
    pub max_commits: usize,
    /// Commits touching more files than this are treated as sweeps.
    pub max_files_per_commit: usize,
    /// Pairs seen in fewer commits than this are coincidence, not coupling.
    pub min_commits: usize,
    /// Ceiling on retained edges, highest weight first.
    pub max_edges: usize,
}

impl Default for CoChangeOptions {
    fn default() -> Self {
        Self {
            max_commits: 500,
            max_files_per_commit: 40,
            min_commits: 2,
            max_edges: 4000,
        }
    }
}

pub struct CoChangeResult {
    pub edges: Vec<CoChangeEdge>,
    pub commits_scanned: usize,
}

/// Mine `repo_root`'s history for pairs of `tracked` files that change together.
///
/// Returns an empty result rather than an error when git is missing, the
/// directory is not a repository, or the history is too short to say anything:
/// co-change is an enrichment, and a graph without it is still a graph.
pub fn mine(
    repo_root: &Path,
    tracked: &BTreeSet<String>,
    options: &CoChangeOptions,
) -> CoChangeResult {
    let empty = CoChangeResult {
        edges: Vec::new(),
        commits_scanned: 0,
    };

    if tracked.is_empty() || options.max_commits == 0 {
        return empty;
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["log", "--no-merges", "--name-only"])
        .arg(COMMIT_FORMAT)
        .arg("-n")
        .arg(options.max_commits.to_string())
        .output();

    let Ok(output) = output else {
        return empty;
    };
    if !output.status.success() {
        return empty;
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return empty;
    };

    let commits = parse_commits(&text, tracked);
    let commits_scanned = commits.len();

    let mut pair_commits: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut pair_weight: BTreeMap<(String, String), f64> = BTreeMap::new();

    for files in &commits {
        let count = files.len();
        if count < 2 || count > options.max_files_per_commit {
            continue;
        }
        // Divide by breadth so every commit contributes comparable mass no
        // matter how many files it happened to touch.
        let share = 1.0 / (count as f64 - 1.0);
        for (index, a) in files.iter().enumerate() {
            for b in files.iter().skip(index + 1) {
                let key = (a.clone(), b.clone());
                *pair_commits.entry(key.clone()).or_insert(0) += 1;
                *pair_weight.entry(key).or_insert(0.0) += share;
            }
        }
    }

    let mut edges: Vec<CoChangeEdge> = pair_commits
        .into_iter()
        .filter(|(_, commits)| *commits >= options.min_commits.max(1))
        .map(|((a, b), commits)| {
            let weight = pair_weight
                .get(&(a.clone(), b.clone()))
                .copied()
                .unwrap_or(0.0);
            CoChangeEdge {
                a,
                b,
                commits,
                weight,
            }
        })
        .collect();

    edges.sort_by(|left, right| {
        right
            .weight
            .partial_cmp(&left.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.a.cmp(&right.a))
            .then_with(|| left.b.cmp(&right.b))
    });
    edges.truncate(options.max_edges);

    CoChangeResult {
        edges,
        commits_scanned,
    }
}

/// Split `git log` output into per-commit file lists, keeping only files the
/// graph actually knows about. Deleted files, vendored paths and anything the
/// manifest excluded would otherwise enter the graph through the back door.
fn parse_commits(text: &str, tracked: &BTreeSet<String>) -> Vec<Vec<String>> {
    let mut commits: Vec<Vec<String>> = Vec::new();
    let mut current: BTreeSet<String> = BTreeSet::new();
    let mut started = false;

    for line in text.lines() {
        if line.starts_with(COMMIT_MARKER) {
            if started {
                commits.push(current.iter().cloned().collect());
            }
            current = BTreeSet::new();
            started = true;
            continue;
        }
        let path = line.trim();
        if path.is_empty() {
            continue;
        }
        if tracked.contains(path) {
            current.insert(path.to_string());
        }
    }

    if started {
        commits.push(current.iter().cloned().collect());
    }

    commits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    fn log(entries: &[(&str, &[&str])]) -> String {
        let mut out = String::new();
        for (hash, files) in entries {
            out.push(COMMIT_MARKER);
            out.push_str(hash);
            out.push('\n');
            for file in *files {
                out.push_str(file);
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn parses_commits_and_drops_untracked_paths() {
        let text = log(&[
            ("aaa", &["src/a.rs", "vendor/x.js"]),
            ("bbb", &["src/b.rs"]),
        ]);
        let commits = parse_commits(&text, &tracked(&["src/a.rs", "src/b.rs"]));
        assert_eq!(
            commits,
            vec![vec!["src/a.rs".to_string()], vec!["src/b.rs".to_string()]]
        );
    }

    #[test]
    fn a_file_listed_twice_in_one_commit_counts_once() {
        let text = log(&[("aaa", &["src/a.rs", "src/a.rs", "src/b.rs"])]);
        let commits = parse_commits(&text, &tracked(&["src/a.rs", "src/b.rs"]));
        assert_eq!(commits[0].len(), 2);
    }

    #[test]
    fn an_empty_log_yields_no_commits() {
        assert!(parse_commits("", &tracked(&["src/a.rs"])).is_empty());
    }

    #[test]
    fn missing_repository_degrades_to_an_empty_result_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let result = mine(
            dir.path(),
            &tracked(&["src/a.rs"]),
            &CoChangeOptions::default(),
        );
        assert!(result.edges.is_empty());
    }
}
