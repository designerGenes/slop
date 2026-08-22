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
//! subfolder will not inherit a parent folder's `.slopignore`. Slop only ever
//! works in one direction — down from the calling folder.
//!
//! `.slopignore` applies to *directory walks* only. A file named explicitly on
//! the command line is always slopped; if you asked for it by name, you meant
//! it. When such a file lives beneath the invocation directory and its own
//! directory has no `.slopignore`, the calling folder's `.slopignore` supplies
//! its `slopinclude` directives — which may themselves point at external
//! directories (e.g. `slopinclude $HOME/shared/`).

use std::fs;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::models::CliArgs;

pub const SLOPIGNORE_FILE_NAME: &str = ".slopignore";

/// A compiled `.slopignore` matcher. An "empty" matcher (no `.slopignore`
/// found, or one that failed to parse) never matches anything, so callers can
/// use it unconditionally.
pub struct SlopIgnore {
    matcher: Option<Gitignore>,
    include_matcher: Option<Gitignore>,
    explicit_includes: Vec<PathBuf>,
    has_explicit_slopignore_include: bool,
    source: Option<PathBuf>,
    heaps: Vec<SlopHeap>,
    option_error: Option<String>,
}

/// A separately rooted group of `.slopignore` rules. Heap ignore patterns are
/// intentionally evaluated after heap includes so a heap can include a broad
/// directory and carve out a local exception.
pub struct SlopHeap {
    matcher: Gitignore,
    include_matcher: Option<Gitignore>,
    explicit_includes: Vec<PathBuf>,
    has_explicit_slopignore_include: bool,
    args: CliArgs,
    ignore_patterns: Vec<String>,
}

struct HeapBuilder {
    root: PathBuf,
    ignore_builder: GitignoreBuilder,
    include_builder: GitignoreBuilder,
    has_local_includes: bool,
    has_explicit_slopignore_include: bool,
    explicit_includes: Vec<PathBuf>,
    ignore_patterns: Vec<String>,
}

impl SlopIgnore {
    pub fn empty() -> Self {
        Self {
            matcher: None,
            include_matcher: None,
            explicit_includes: Vec::new(),
            has_explicit_slopignore_include: false,
            source: None,
            heaps: Vec::new(),
            option_error: None,
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
        let mut has_explicit_slopignore_include = false;
        let mut explicit_includes = Vec::new();
        let mut heaps = Vec::new();
        let mut heap_stack = Vec::new();
        for line in contents.lines() {
            let trimmed = line.trim();
            if let Some(path) = trimmed.strip_prefix(r"\/") {
                let path = crate::pathing::expand_tilde(Path::new(path.trim()));
                if !path.is_absolute() {
                    eprintln!(
                        "warning: slopheap in {} must use an absolute path; continuing without it",
                        source.display()
                    );
                    return Self::empty();
                }
                heap_stack.push(HeapBuilder::new(crate::pathing::normalize_path(&path)));
            } else if let Some(options) = heap_close_options(trimmed) {
                let Some(heap) = heap_stack.pop() else {
                    eprintln!(
                        "warning: slopheap close without an opening directive in {}; continuing without it",
                        source.display()
                    );
                    return Self::empty();
                };
                let args = match crate::cli::parse_slopheap_options(options, &heap.root) {
                    Ok(args) => args,
                    Err(error) => {
                        let message = format!(
                            "could not parse slopheap options in {}: {error}",
                            source.display()
                        );
                        eprintln!("warning: {message}");
                        return Self::with_option_error(message);
                    }
                };
                match heap.build(&source, args) {
                    Ok(heap) => heaps.push(heap),
                    Err(error) => {
                        eprintln!("warning: {error}; continuing without it");
                        return Self::empty();
                    }
                }
            } else if let Some(heap) = heap_stack.last_mut() {
                if let Err(error) = heap.add_line(line, &source) {
                    eprintln!("warning: {error}; continuing without it");
                    return Self::empty();
                }
            } else if let Some(pattern) = include_pattern(line) {
                let expanded = crate::pathing::expand_tilde(Path::new(pattern));
                if expanded.is_absolute() {
                    let norm = crate::pathing::normalize_path(&expanded);
                    if norm.file_name().and_then(|n| n.to_str()) == Some(SLOPIGNORE_FILE_NAME) {
                        has_explicit_slopignore_include = true;
                    }
                    explicit_includes.push(norm);
                } else {
                    let clean_pattern = pattern.strip_prefix("./").unwrap_or(pattern);
                    if clean_pattern.trim().ends_with(SLOPIGNORE_FILE_NAME) {
                        has_explicit_slopignore_include = true;
                    }
                    if let Err(error) =
                        include_builder.add_line(Some(source.clone()), clean_pattern)
                    {
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

        if !heap_stack.is_empty() {
            eprintln!(
                "warning: slopheap in {} is missing a closing /\\ directive; continuing without it",
                source.display()
            );
            return Self::empty();
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
            has_explicit_slopignore_include,
            source: Some(source),
            heaps,
            option_error: None,
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

    /// Whether `path` (a `.slopignore` file) was explicitly targeted by a `slopinclude` directive.
    pub fn is_explicit_slopignore_included(&self, path: &Path) -> bool {
        if self.explicit_includes.contains(&path.to_path_buf()) {
            return true;
        }
        self.has_explicit_slopignore_include
    }

    /// Directory that local include rules are evaluated beneath.
    pub fn include_root(&self) -> Option<&Path> {
        self.include_matcher.as_ref().map(|matcher| matcher.path())
    }

    /// Absolute files named by `slopinclude` directives.
    pub fn explicit_includes(&self) -> &[PathBuf] {
        &self.explicit_includes
    }

    /// Separately rooted directive groups declared between `\/` and `/\`.
    pub fn heaps(&self) -> &[SlopHeap] {
        &self.heaps
    }

    pub fn option_error(&self) -> Option<&str> {
        self.option_error.as_deref()
    }

    fn with_option_error(error: String) -> Self {
        let mut ignore = Self::empty();
        ignore.option_error = Some(error);
        ignore
    }
}

impl HeapBuilder {
    fn new(root: PathBuf) -> Self {
        Self {
            ignore_builder: GitignoreBuilder::new(&root),
            include_builder: GitignoreBuilder::new(&root),
            root,
            has_local_includes: false,
            has_explicit_slopignore_include: false,
            explicit_includes: Vec::new(),
            ignore_patterns: Vec::new(),
        }
    }

    fn add_line(&mut self, line: &str, source: &Path) -> Result<(), String> {
        let heap_source = self.root.join(SLOPIGNORE_FILE_NAME);
        if let Some(pattern) = include_pattern(line) {
            let expanded = crate::pathing::expand_tilde(Path::new(pattern));
            if expanded.is_absolute() {
                let path = crate::pathing::normalize_path(&expanded);
                if path.file_name().and_then(|name| name.to_str()) == Some(SLOPIGNORE_FILE_NAME) {
                    self.has_explicit_slopignore_include = true;
                }
                self.explicit_includes.push(path);
            } else {
                let pattern = pattern.strip_prefix("./").unwrap_or(pattern);
                if pattern.trim().ends_with(SLOPIGNORE_FILE_NAME) {
                    self.has_explicit_slopignore_include = true;
                }
                self.include_builder
                    .add_line(Some(heap_source), pattern)
                    .map_err(|error| {
                        format!(
                            "could not compile slopheap include in {}: {error}",
                            source.display()
                        )
                    })?;
                self.has_local_includes = true;
            }
        } else {
            self.ignore_builder
                .add_line(Some(heap_source), line.trim_start())
                .map_err(|error| {
                    format!(
                        "could not compile slopheap rule in {}: {error}",
                        source.display()
                    )
                })?;
            self.ignore_patterns.push(line.trim_start().to_string());
        }
        Ok(())
    }

    fn build(self, source: &Path, args: CliArgs) -> Result<SlopHeap, String> {
        let matcher = self.ignore_builder.build().map_err(|error| {
            format!(
                "could not compile slopheap in {}: {error}",
                source.display()
            )
        })?;
        let include_matcher = if self.has_local_includes {
            Some(self.include_builder.build().map_err(|error| {
                format!(
                    "could not compile slopheap include in {}: {error}",
                    source.display()
                )
            })?)
        } else {
            None
        };
        Ok(SlopHeap {
            matcher,
            include_matcher,
            explicit_includes: self.explicit_includes,
            has_explicit_slopignore_include: self.has_explicit_slopignore_include,
            args,
            ignore_patterns: self.ignore_patterns,
        })
    }
}

impl SlopHeap {
    pub fn root(&self) -> &Path {
        self.matcher.path()
    }

    pub fn args(&self) -> &CliArgs {
        &self.args
    }

    pub fn ignore_patterns(&self) -> &[String] {
        &self.ignore_patterns
    }

    pub fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        let relative = path.strip_prefix(self.matcher.path()).unwrap_or(path);
        self.matcher.matched(relative, is_dir).is_ignore()
    }

    pub fn is_included(&self, path: &Path, is_dir: bool) -> bool {
        let Some(matcher) = self.include_matcher.as_ref() else {
            return false;
        };
        let relative = path.strip_prefix(matcher.path()).unwrap_or(path);
        matcher
            .matched_path_or_any_parents(relative, is_dir)
            .is_ignore()
    }

    pub fn include_root(&self) -> Option<&Path> {
        self.include_matcher.as_ref().map(|matcher| matcher.path())
    }

    pub fn explicit_includes(&self) -> &[PathBuf] {
        &self.explicit_includes
    }

    pub fn is_explicit_slopignore_included(&self, path: &Path) -> bool {
        self.explicit_includes.contains(&path.to_path_buf()) || self.has_explicit_slopignore_include
    }
}

/// Match a bare heap close or one followed by whitespace-separated CLI
/// options. A path-like ignore pattern beginning with `/\` remains a pattern.
fn heap_close_options(line: &str) -> Option<&str> {
    let rest = line.strip_prefix(r"/\")?;
    if rest.is_empty() {
        return Some(rest);
    }
    rest.chars()
        .next()
        .is_some_and(char::is_whitespace)
        .then_some(rest.trim())
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
///
/// One exception, for explicit *file* inputs only: a file named on the command
/// line is always slopped regardless of ignore rules, so the only directives
/// that matter for it are `slopinclude`s. When the file lives beneath the
/// invocation directory and its own directory has no `.slopignore`, the calling
/// folder's `.slopignore` supplies them. Directory walks never get this
/// fallback — they reflect the walked folder and nothing above it.
fn find_slopignore(input: &Path) -> Option<PathBuf> {
    let start = start_dir(input)?;
    let candidate = start.join(SLOPIGNORE_FILE_NAME);
    if candidate.is_file() {
        return Some(candidate);
    }

    if input.is_file() {
        let cwd = std::env::current_dir().ok()?;
        // Canonicalize for the comparison only: on macOS the walker-facing
        // paths stay in their original (/var/...) spelling, while `getcwd`
        // returns the physical (/private/var/...) path.
        let start = start.canonicalize().unwrap_or_else(|_| start.clone());
        if start != cwd && start.starts_with(&cwd) {
            let candidate = cwd.join(SLOPIGNORE_FILE_NAME);
            if candidate.is_file() {
                return Some(candidate);
            }
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
    fn heap_patterns_are_rooted_in_the_heap_directory() {
        let temp = tempdir().expect("tempdir");
        let pocket = temp.path().join("pocket");
        let heap_root = temp.path().join("heap");
        fs::create_dir_all(heap_root.join("src")).expect("heap root");
        fs::create_dir_all(&pocket).expect("pocket");
        fs::write(
            pocket.join(SLOPIGNORE_FILE_NAME),
            format!(
                "\\/ {}\n+ src/\nsrc/excluded.rs\n/\\\n",
                heap_root.display()
            ),
        )
        .expect("slopignore");

        let ignore = SlopIgnore::discover(&pocket);
        let heap = ignore.heaps().first().expect("heap should be parsed");
        assert!(heap.is_included(&heap_root.join("src/keep.rs"), false));
        assert!(heap.is_ignored(&heap_root.join("src/excluded.rs"), false));
    }

    #[test]
    fn heap_close_parses_standard_cli_options() {
        let temp = tempdir().expect("tempdir");
        let pocket = temp.path().join("pocket");
        let heap_root = temp.path().join("heap");
        fs::create_dir_all(&heap_root).expect("heap root");
        fs::create_dir_all(&pocket).expect("pocket");
        fs::write(
            pocket.join(SLOPIGNORE_FILE_NAME),
            format!(
                "\\/ {}\n+ src/\n/\\ -g --graph-map-tokens 512 -x '*.tmp'\n",
                heap_root.display()
            ),
        )
        .expect("slopignore");

        let ignore = SlopIgnore::discover(&pocket);
        let heap = ignore.heaps().first().expect("heap should be parsed");
        assert_eq!(heap.root(), heap_root);
        assert!(heap.args().include_graph);
        assert_eq!(heap.args().graph_map_tokens, Some(512));
        assert_eq!(heap.args().exclude, vec!["*.tmp"]);
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
