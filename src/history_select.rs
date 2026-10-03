//! Task-conditioned selection from repository history.
//!
//! Which files does adding a new X touch? The repository has already answered
//! that every time someone added the previous X. For a seed file, take the
//! commits before the base that ADDED a file in the seed's own directory (its
//! siblings), and tally the files those commits MODIFIED. Registries that a
//! new rule must register with recur almost every time; incidental files do
//! not.
//!
//! This is not the general co-change miner in [`crate::graphstore::cochange`],
//! which averages coupling over all history. This query is conditioned on the
//! seed's sibling-addition pattern at the revision the page opens against.
//!
//! Preconditions: a git repository with a usable history and a directory
//! convention (siblings live beside the seed). When either is absent — a
//! shallow clone, a fresh directory with no prior additions — the caller
//! falls back to the qualifier scorer and then to legacy selection.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

/// Files modified when prior siblings were added, and the denominator.
pub struct HistorySelection {
    /// Repo-relative path -> number of prior sibling-addition commits that
    /// modified it with status `M`.
    pub tally: BTreeMap<String, usize>,
    /// Prior sibling-addition commits considered.
    pub prior_commits: usize,
}

/// The minimum number of agreeing prior sibling-additions for a file to be a
/// history candidate.
///
/// One prior commit is an anecdote; two agreeing commits is the smallest
/// evidence that a convention exists. This is also what keeps a mass-move
/// commit (a crate rename that "adds" a whole directory at once) from
/// producing a bogus 1/1 tally — nothing reaches two, so the signal yields
/// nothing and the caller falls back.
pub const MIN_AGREEING_COMMITS: usize = 2;

impl HistorySelection {
    /// How many prior sibling-additions modified `rel`.
    pub fn count(&self, rel: &str) -> usize {
        self.tally.get(rel).copied().unwrap_or(0)
    }

    /// Whether `rel` has enough agreeing commits to be a candidate.
    pub fn meets_minimum(&self, rel: &str) -> bool {
        self.count(rel) >= MIN_AGREEING_COMMITS
    }
}

/// The directory scope a prior addition must fall in.
#[derive(Clone, Debug)]
enum AddScope {
    /// Files added directly in the seed's own directory (ruff's shape:
    /// `rules/<plugin>/rules/*.rs`).
    OwnDir(String),
    /// Files added one level below the seed's parent, for directory-per-item
    /// conventions (gh's shape: `pkg/cmd/<group>/<verb>/<verb>.go`), where the
    /// seed's own directory has only ever had its own addition.
    ParentDir(String),
}

impl AddScope {
    fn pathspec(&self, ext: &str) -> String {
        match self {
            AddScope::OwnDir(dir) if dir == "." => format!("*.{ext}"),
            AddScope::OwnDir(dir) => format!("{dir}/*.{ext}"),
            AddScope::ParentDir(dir) if dir == "." => format!("*/*.{ext}"),
            AddScope::ParentDir(dir) => format!("{dir}/*/*.{ext}"),
        }
    }

    /// Whether an added path belongs to this scope. The pathspec may
    /// over-match on some git versions, so every commit is verified against
    /// its own name-status output.
    fn accepts(&self, path: &str, ext: &str) -> bool {
        if !path.ends_with(&format!(".{ext}")) {
            return false;
        }
        let parent = parent_directory(path);
        match self {
            AddScope::OwnDir(dir) => parent == dir,
            AddScope::ParentDir(dir) => parent_directory(parent) == dir,
        }
    }
}

fn parent_directory(path: &str) -> &str {
    path.rsplit_once('/')
        .map_or(".", |(directory, _)| directory)
}

/// Files that exist at `base`, used to discard modifications of paths that no
/// longer exist. `None` means the tree could not be listed; the guard is then
/// unavailable and everything is tallied.
fn base_tree(repo_root: &Path, base: &str) -> Option<BTreeSet<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["ls-tree", "-r", "--name-only", base])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|text| text.lines().map(str::to_string).collect())
}

/// Mine git history for the files prior sibling additions modified.
///
/// Returns `None` when history is unusable: `window` is 0, the seed has no
/// extension, the directory is not a repository, git is missing, no commits
/// exist, or no verifiable sibling additions were found. A `None` is not an
/// error — selection then falls back.
pub fn sibling_additions(
    repo_root: &Path,
    seed_rel: &str,
    window: usize,
    base: Option<&str>,
) -> Option<HistorySelection> {
    if window == 0 {
        return None;
    }
    let ext = Path::new(seed_rel).extension()?.to_str()?.to_string();
    let sibling_dir = parent_directory(seed_rel);
    let base = base.unwrap_or("HEAD");
    let known = base_tree(repo_root, base);

    let own = scan_scope(
        repo_root,
        base,
        window,
        &ext,
        &AddScope::OwnDir(sibling_dir.to_string()),
        &known,
    );
    // A directory-per-item convention (each command in its own directory)
    // leaves the seed's own directory with a single addition. Retry one level
    // up so sibling *commands* count as the prior, not sibling files.
    if own
        .as_ref()
        .is_none_or(|result| result.prior_commits < MIN_AGREEING_COMMITS)
        && let Some((parent, _)) = sibling_dir.rsplit_once('/')
    {
        if let Some(widened) = scan_scope(
            repo_root,
            base,
            window,
            &ext,
            &AddScope::ParentDir(parent.to_string()),
            &known,
        ) && widened.prior_commits > 0
        {
            return Some(widened);
        }
    }
    own.filter(|result| result.prior_commits > 0)
}

/// One scan over a scope's addition commits. Returns `None` only for a git
/// failure; a scope with no verifiable additions yields an empty selection.
fn scan_scope(
    repo_root: &Path,
    base: &str,
    window: usize,
    ext: &str,
    scope: &AddScope,
    known: &Option<BTreeSet<String>>,
) -> Option<HistorySelection> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args([
            "log",
            base,
            "--no-merges",
            "--diff-filter=A",
            "--format=%H",
            "-n",
        ])
        .arg(window.to_string())
        .arg("--")
        .arg(scope.pathspec(ext))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commits: Vec<String> = String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();

    let mut tally: BTreeMap<String, usize> = BTreeMap::new();
    let mut prior_commits = 0usize;
    for commit in &commits {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["show", "--no-renames", "--name-status", "--format=", commit])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let Ok(text) = String::from_utf8(output.stdout) else {
            return None;
        };
        let mut added_sibling = false;
        let mut modified: Vec<String> = Vec::new();
        for line in text.lines() {
            let Some((status, path)) = line.split_once('\t') else {
                continue;
            };
            match status.chars().next() {
                Some('A') => {
                    if scope.accepts(path, ext) {
                        added_sibling = true;
                    }
                }
                Some('M') => {
                    if path.ends_with(&format!(".{ext}")) {
                        modified.push(path.to_string());
                    }
                }
                _ => {}
            }
        }
        if !added_sibling {
            continue;
        }
        prior_commits += 1;
        for path in modified {
            // Staleness guard: a path that does not exist at the base commit
            // cannot be a file this task needs to touch. Refactors leave
            // dead paths in old commit diffs (gh's command tree moved), and
            // tallying them points the page at files that are gone.
            if known.as_ref().is_some_and(|files| !files.contains(&path)) {
                continue;
            }
            *tally.entry(path).or_insert(0) += 1;
        }
    }
    Some(HistorySelection {
        tally,
        prior_commits,
    })
}

#[cfg(test)]
mod tests {
    use super::sibling_additions;
    use std::path::Path;
    use std::process::Command;

    fn write(dir: &Path, rel: &str, contents: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, contents).expect("write");
    }

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    fn commit(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-m", message]);
    }

    #[test]
    fn tallies_files_modified_by_prior_sibling_additions() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        write(dir.path(), "README.md", "x\n");
        commit(dir.path(), "init");
        write(dir.path(), "rules/seed.rs", "seed\n");
        write(dir.path(), "rules/mod.rs", "mod seed\n");
        write(dir.path(), "codes.rs", "v1\n");
        commit(dir.path(), "add seed");
        write(dir.path(), "rules/sibling_a.rs", "a\n");
        write(dir.path(), "codes.rs", "v2\n");
        write(dir.path(), "rules/mod.rs", "mod a\n");
        commit(dir.path(), "add sibling a");
        write(dir.path(), "rules/sibling_b.rs", "b\n");
        write(dir.path(), "codes.rs", "v3\n");
        write(dir.path(), "rules/mod.rs", "mod a\nmod b\n");
        commit(dir.path(), "add sibling b");

        let history = sibling_additions(dir.path(), "rules/seed.rs", 8, None).expect("history");
        assert_eq!(history.prior_commits, 3);
        assert_eq!(history.tally.get("codes.rs"), Some(&2));
        assert_eq!(history.tally.get("rules/mod.rs"), Some(&2));
        assert!(history.meets_minimum("codes.rs"));
        assert!(!history.meets_minimum("README.md"));
    }

    #[test]
    fn a_single_prior_addition_is_not_enough() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        write(dir.path(), "README.md", "x\n");
        write(dir.path(), "codes.rs", "v0\n");
        commit(dir.path(), "init");
        write(dir.path(), "rules/seed.rs", "seed\n");
        write(dir.path(), "codes.rs", "v1\n");
        commit(dir.path(), "add seed");
        let history = sibling_additions(dir.path(), "rules/seed.rs", 8, None).expect("history");
        assert_eq!(history.prior_commits, 1);
        assert_eq!(history.count("codes.rs"), 1);
        assert!(
            !history.meets_minimum("codes.rs"),
            "one agreeing commit is an anecdote, not a convention"
        );
    }

    #[test]
    fn widens_to_the_parent_directory_when_the_seed_dir_is_thin() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        write(dir.path(), "README.md", "x\n");
        commit(dir.path(), "init");
        write(dir.path(), "pkg/cmd/repo/archive/archive.go", "archive\n");
        write(dir.path(), "pkg/cmd/repo/repo.go", "v1\n");
        commit(dir.path(), "add archive");
        write(
            dir.path(),
            "pkg/cmd/repo/unarchive/unarchive.go",
            "unarchive\n",
        );
        write(dir.path(), "pkg/cmd/repo/repo.go", "v2\n");
        commit(dir.path(), "add unarchive");
        write(dir.path(), "pkg/cmd/repo/rename/rename.go", "rename\n");
        write(dir.path(), "pkg/cmd/repo/repo.go", "v3\n");
        commit(dir.path(), "add rename");

        let history = sibling_additions(dir.path(), "pkg/cmd/repo/archive/archive.go", 8, None)
            .expect("history");
        // Own directory has one prior (itself); the widened parent scope sees
        // all three command additions.
        assert_eq!(history.prior_commits, 3);
        assert_eq!(history.count("pkg/cmd/repo/repo.go"), 2);
        assert!(history.meets_minimum("pkg/cmd/repo/repo.go"));
    }

    #[test]
    fn stale_paths_are_not_tallied() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        write(dir.path(), "README.md", "x\n");
        write(dir.path(), "codes.rs", "v0\n");
        commit(dir.path(), "init");
        write(dir.path(), "old/registry.go", "v1\n");
        write(dir.path(), "rules/seed.rs", "seed\n");
        write(dir.path(), "codes.rs", "v1\n");
        commit(dir.path(), "add seed");
        write(dir.path(), "rules/sibling.rs", "s\n");
        write(dir.path(), "codes.rs", "v2\n");
        write(dir.path(), "old/registry.go", "v2\n");
        commit(dir.path(), "add sibling");
        std::fs::remove_file(dir.path().join("old/registry.go")).expect("remove");
        commit(dir.path(), "refactor away the old registry");

        let history = sibling_additions(dir.path(), "rules/seed.rs", 8, None).expect("history");
        assert_eq!(history.count("codes.rs"), 2);
        assert!(
            !history.tally.contains_key("old/registry.go"),
            "a path that does not exist at the base must not be tallied"
        );
    }

    #[test]
    fn additions_outside_the_seed_directory_do_not_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q"]);
        write(dir.path(), "README.md", "x\n");
        commit(dir.path(), "init");
        write(dir.path(), "rules/seed.rs", "seed\n");
        commit(dir.path(), "add seed");
        // A nested addition with a modification must not enter the tally.
        write(dir.path(), "rules/sub/deep.rs", "deep\n");
        write(dir.path(), "codes.rs", "v1\n");
        commit(dir.path(), "add nested");
        let history = sibling_additions(dir.path(), "rules/seed.rs", 8, None).expect("history");
        assert_eq!(history.prior_commits, 1);
        assert!(history.tally.get("codes.rs").is_none());
    }

    #[test]
    fn no_repository_or_no_window_yields_none() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(sibling_additions(dir.path(), "rules/seed.rs", 8, None).is_none());
        git(dir.path(), &["init", "-q"]);
        write(dir.path(), "rules/seed.rs", "seed\n");
        commit(dir.path(), "add seed");
        assert!(sibling_additions(dir.path(), "rules/seed.rs", 0, None).is_none());
    }
}
