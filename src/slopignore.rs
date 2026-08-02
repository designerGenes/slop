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
//! `.slopignore` in the target walked directory (or the directory of a target file).
//! Ancestor directories are deliberately not searched, so invoking slop within a
//! subfolder will not inherit a parent folder's `.slopignore`.
//!
//! `.slopignore` applies to *directory walks* only. A file named explicitly on
//! the command line is always slopped; if you asked for it by name, you meant
//! it. Its `slopinclude` directives still apply when that file is beneath the
//! directory where the command was invoked.

use std::fs;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

pub const SLOPIGNORE_FILE_NAME: &str = ".slopignore";

/// A compiled `.slopignore` matcher. An "empty" matcher (no `.slopignore`
/// found, or one that failed to parse) never matches anything, so callers can
/// use it unconditionally.
pub struct SlopIgnore {
    matcher: Option<Gitignore>,
    include_matcher: Option<Gitignore>,
    explicit_includes: Vec<PathBuf>,
    source: Option<PathBuf>,
}

impl SlopIgnore {
    pub fn empty() -> Self {
        Self {
            matcher: None,
            include_matcher: None,
            explicit_includes: Vec::new(),
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

        let contents = match fs::read_to_string(&source) {
            Ok(contents) => contents,
            Err(error) => {
                eprintln!(
                    "warning: could not read {}: {error}; continuing without it",
                    source.display()
                );
                return Self::empty();
            }
        };

        let mut builder = GitignoreBuilder::new(root);
        let mut include_builder = GitignoreBuilder::new(root);
        let mut has_local_includes = false;
        let mut explicit_includes = Vec::new();
        for line in contents.lines() {
            if let Some(pattern) = include_pattern(line) {
                let expanded = crate::pathing::expand_tilde(Path::new(pattern));
                if expanded.is_absolute() {
                    explicit_includes.push(crate::pathing::normalize_path(&expanded));
                } else {
                    let clean_pattern = pattern.strip_prefix("./").unwrap_or(pattern);
                    if let Err(error) = include_builder.add_line(Some(source.clone()), clean_pattern) {
                        eprintln!(
                            "warning: could not compile slopinclude in {}: {error}; continuing without it",
                            source.display()
                        );
                        return Self::empty();
                    }
                    has_local_includes = true;
                }
            } else if let Err(error) = builder.add_line(Some(source.clone()), line) {
                eprintln!(
                    "warning: could not read {}: {error}; continuing without it",
                    source.display()
                );
                return Self::empty();
            }
        }

        let matcher = match builder.build() {
            Ok(matcher) => matcher,
            Err(error) => {
                eprintln!(
                    "warning: could not compile {}: {error}; continuing without it",
                    source.display()
                );
                return Self::empty();
            }
        };
        let include_matcher = if has_local_includes {
            match include_builder.build() {
                Ok(matcher) => Some(matcher),
                Err(error) => {
                    eprintln!(
                        "warning: could not compile slopinclude in {}: {error}; continuing without it",
                        source.display()
                    );
                    return Self::empty();
                }
            }
        } else {
            None
        };

        Self {
            matcher: Some(matcher),
            include_matcher,
            explicit_includes,
            source: Some(source),
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

    /// Whether a local `slopinclude` directive matches `path`. Include rules
    /// use the same gitignore-style syntax and root as ordinary ignore rules.
    pub fn is_included(&self, path: &Path, is_dir: bool) -> bool {
        let Some(matcher) = self.include_matcher.as_ref() else {
            return false;
        };

        let relative = path.strip_prefix(matcher.path()).unwrap_or(path);
        matcher
            .matched_path_or_any_parents(relative, is_dir)
            .is_ignore()
    }

    /// Directory that local include rules are evaluated beneath.
    pub fn include_root(&self) -> Option<&Path> {
        self.include_matcher.as_ref().map(|matcher| matcher.path())
    }

    /// Absolute files named by `slopinclude` directives.
    pub fn explicit_includes(&self) -> &[PathBuf] {
        &self.explicit_includes
    }
}

/// Extract the pattern from either supported include-directive spelling.
fn include_pattern(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if let Some(pattern) = trimmed.strip_prefix('+') {
        return (!pattern.trim().is_empty()).then_some(pattern.trim_start());
    }

    let pattern = trimmed.strip_prefix("slopinclude")?;
    if !pattern.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    (!pattern.trim().is_empty()).then_some(pattern.trim_start())
}

/// Locate the `.slopignore` governing `input`.
/// Only checks the directory of `input` directly; ancestor directories
/// are not searched so that subfolder invocations do not inherit parent `.slopignore` files.
fn find_slopignore(input: &Path) -> Option<PathBuf> {
    let start = start_dir(input)?;
    let candidate = start.join(SLOPIGNORE_FILE_NAME);
    if candidate.is_file() {
        Some(candidate)
    } else {
        None
    }
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
        fs::write(root.join(SLOPIGNORE_FILE_NAME), "*.md\n!README.md\n")
            .expect("slopignore should be written");

        let ignore = SlopIgnore::discover(root);
        assert!(ignore.is_ignored(&root.join("NOTES.md"), false));
        assert!(!ignore.is_ignored(&root.join("README.md"), false));
    }

    #[test]
    fn parses_both_slopinclude_directive_spellings() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::write(
            root.join(SLOPIGNORE_FILE_NAME),
            "*.md\n+ forced.md\nslopinclude nested/also.md\n",
        )
        .expect("slopignore should be written");

        let ignore = SlopIgnore::discover(root);
        assert!(ignore.is_ignored(&root.join("forced.md"), false));
        assert!(ignore.is_included(&root.join("forced.md"), false));
        assert!(ignore.is_included(&root.join("nested/also.md"), false));
        assert!(
            !ignore.is_ignored(&root.join("slopinclude"), false),
            "the directive must not be compiled as an ordinary ignore rule"
        );
    }

    #[test]
    fn does_not_climb_to_parent_repo_root() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        fs::create_dir_all(root.join(".git")).expect("git marker should be created");
        fs::create_dir_all(root.join("src/nested")).expect("dirs should be created");
        fs::write(root.join(SLOPIGNORE_FILE_NAME), "*.log\n")
            .expect("slopignore should be written");

        let ignore = SlopIgnore::discover(&root.join("src/nested"));
        assert!(
            !ignore.is_active(),
            "a parent repo-root .slopignore should not govern nested subfolder walks"
        );
        assert!(!ignore.is_ignored(&root.join("src/nested/debug.log"), false));
    }

    #[test]
    fn subfolder_slopignore_governs_only_subfolder() {
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
            "subfolder .slopignore governs subfolder walk, root .slopignore is not consulted"
        );
    }
}
