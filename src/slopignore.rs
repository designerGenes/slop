//! `.slopignore` support.
//!
//! A `.slopignore` file uses exactly the same pattern syntax as `.gitignore`
//! (it is compiled by the same `ignore::gitignore` engine), but it means
//! something different: "files slop should not bundle", *not* "files git
//! should not track". The two are deliberately independent — a repo can
//! ignore `target/` for git and `docs/vendor/` for slop without either
//! leaking into the other.
//!
//! Discovery is intentionally narrow and predictable: slop looks for a single
//! `.slopignore`, starting at the walked directory and climbing ancestors only
//! while still inside the containing git repository (stopping at, and
//! including, the repo root). Outside a git repo, only the walked directory
//! itself is consulted — slop will never silently pick up a stray
//! `.slopignore` from `$HOME` or `/`.
//!
//! `.slopignore` applies to *directory walks* only. A file named explicitly on
//! the command line is always slopped; if you asked for it by name, you meant
//! it.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

pub const SLOPIGNORE_FILE_NAME: &str = ".slopignore";

/// A compiled `.slopignore` matcher. An "empty" matcher (no `.slopignore`
/// found, or one that failed to parse) never matches anything, so callers can
/// use it unconditionally.
pub struct SlopIgnore {
    matcher: Option<Gitignore>,
    source: Option<PathBuf>,
}

impl SlopIgnore {
    pub fn empty() -> Self {
        Self {
            matcher: None,
            source: None,
        }
    }

    /// Locate and compile the `.slopignore` governing `input`.
    ///
    /// A malformed `.slopignore` is a warning, never a hard error: slop should
    /// still produce a bundle when the ignore file is broken.
    pub fn discover(input: &Path) -> Self {
        let Some(source) = find_slopignore(input) else {
            return Self::empty();
        };

        let Some(root) = source.parent() else {
            return Self::empty();
        };

        let mut builder = GitignoreBuilder::new(root);
        if let Some(error) = builder.add(&source) {
            eprintln!(
                "warning: could not read {}: {error}; continuing without it",
                source.display()
            );
            return Self::empty();
        }

        match builder.build() {
            Ok(matcher) => Self {
                matcher: Some(matcher),
                source: Some(source),
            },
            Err(error) => {
                eprintln!(
                    "warning: could not compile {}: {error}; continuing without it",
                    source.display()
                );
                Self::empty()
            }
        }
    }

    /// Whether a `.slopignore` was found and compiled.
    pub fn is_active(&self) -> bool {
        self.matcher.is_some()
    }

    /// Path of the `.slopignore` in force, for `--verbose` reporting.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Whether `path` is excluded by the active `.slopignore`.
    ///
    /// `is_dir` matters: `build/` only matches directories, while `*.log` is
    /// evaluated against files. Paths are matched relative to the directory
    /// holding the `.slopignore`, matching git's semantics.
    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let Some(matcher) = self.matcher.as_ref() else {
            return false;
        };

        let relative = path.strip_prefix(matcher.path()).unwrap_or(path);
        matcher.matched(relative, is_dir).is_ignore()
    }
}

/// Walk from `input` up to the containing git root looking for a
/// `.slopignore`. Returns the nearest one, or `None`.
fn find_slopignore(input: &Path) -> Option<PathBuf> {
    let start = start_dir(input)?;
    let git_root = crate::pathing::containing_git_root(&start);

    let mut current: Option<&Path> = Some(start.as_path());
    while let Some(dir) = current {
        let candidate = dir.join(SLOPIGNORE_FILE_NAME);
        if candidate.is_file() {
            return Some(candidate);
        }

        match git_root.as_deref() {
            // Reached the repo root without a hit: stop, do not escape the repo.
            Some(root) if is_same_dir(dir, root) => return None,
            Some(_) => current = dir.parent(),
            // Not inside a repo: consult only the directory we were pointed at.
            None => return None,
        }
    }

    None
}

/// Deliberately *not* canonicalized: the returned path becomes the matcher's
/// root, and it must stay in the same form as the paths the walker yields.
/// On macOS a canonicalized `/var/folders/...` becomes `/private/var/...`,
/// and every `strip_prefix` against walker output would then miss.
fn start_dir(input: &Path) -> Option<PathBuf> {
    if input.is_dir() {
        Some(input.to_path_buf())
    } else {
        Some(input.parent()?.to_path_buf())
    }
}

/// `containing_git_root` canonicalizes; the climb does not. Compare both ways
/// so the bound still holds on platforms where temp dirs are symlinked.
fn is_same_dir(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn empty_matcher_ignores_nothing() {
        let ignore = SlopIgnore::empty();
        assert!(!ignore.is_active());
        assert!(!ignore.is_ignored(Path::new("/tmp/anything.rs"), false));
    }

    #[test]
    fn missing_slopignore_yields_empty_matcher() {
        let temp = tempdir().expect("tempdir");
        let ignore = SlopIgnore::discover(temp.path());
        assert!(!ignore.is_active());
    }

    #[test]
    fn matches_file_and_directory_patterns() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::write(root.join(SLOPIGNORE_FILE_NAME), "*.log\nvendor/\n")
            .expect("slopignore should be written");

        let ignore = SlopIgnore::discover(root);
        assert!(ignore.is_active());
        assert!(ignore.is_ignored(&root.join("debug.log"), false));
        assert!(ignore.is_ignored(&root.join("vendor"), true));
        assert!(!ignore.is_ignored(&root.join("main.rs"), false));
        // `vendor/` is a directory pattern; a *file* of that name is kept.
        assert!(!ignore.is_ignored(&root.join("vendor"), false));
    }

    #[test]
    fn supports_negation_like_gitignore() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::write(
            root.join(SLOPIGNORE_FILE_NAME),
            "*.md\n!README.md\n",
        )
        .expect("slopignore should be written");

        let ignore = SlopIgnore::discover(root);
        assert!(ignore.is_ignored(&root.join("NOTES.md"), false));
        assert!(!ignore.is_ignored(&root.join("README.md"), false));
    }

    #[test]
    fn climbs_to_repo_root_when_inside_a_git_repo() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::create_dir_all(root.join(".git")).expect("git marker should be created");
        fs::create_dir_all(root.join("src/nested")).expect("dirs should be created");
        fs::write(root.join(SLOPIGNORE_FILE_NAME), "*.log\n")
            .expect("slopignore should be written");

        let ignore = SlopIgnore::discover(&root.join("src/nested"));
        assert!(
            ignore.is_active(),
            "a repo-root .slopignore should govern nested walks"
        );
        assert!(ignore.is_ignored(&root.join("src/nested/debug.log"), false));
    }

    #[test]
    fn does_not_climb_outside_a_git_repo() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::create_dir_all(root.join("plain/nested")).expect("dirs should be created");
        fs::write(root.join(SLOPIGNORE_FILE_NAME), "*.log\n")
            .expect("slopignore should be written");

        // `plain/nested` is not in a git repo, so the ancestor .slopignore is
        // deliberately not consulted.
        let ignore = SlopIgnore::discover(&root.join("plain/nested"));
        assert!(!ignore.is_active());
    }

    #[test]
    fn nearest_slopignore_wins() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::create_dir_all(root.join(".git")).expect("git marker should be created");
        fs::create_dir_all(root.join("src")).expect("dirs should be created");
        fs::write(root.join(SLOPIGNORE_FILE_NAME), "*.log\n").expect("root slopignore");
        fs::write(root.join("src").join(SLOPIGNORE_FILE_NAME), "*.tmp\n")
            .expect("nested slopignore");

        let ignore = SlopIgnore::discover(&root.join("src"));
        assert!(ignore.is_ignored(&root.join("src/scratch.tmp"), false));
        assert!(
            !ignore.is_ignored(&root.join("src/debug.log"), false),
            "the nearest .slopignore replaces ancestors rather than layering"
        );
    }
}
