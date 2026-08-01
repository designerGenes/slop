use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use ignore::WalkBuilder;
use regex::Regex;
use walkdir::WalkDir;

use crate::config::Config;
use crate::error::SlopError;
use crate::models::{IgnoreReason, IgnoredEntry, WalkReport};
use crate::slopignore::SlopIgnore;

/// Shared sink for `.slopignore` hits. The `ignore` crate requires its
/// `filter_entry` closure to be `Fn + Send + Sync + 'static`, so the sink and
/// the matcher both have to be owned by the closure rather than borrowed.
type IgnoredSink = Arc<Mutex<Vec<IgnoredEntry>>>;

// VCS/build/output trees are never useful in a slop and routinely hold
// binary blobs (git loose objects, compiled artifacts) that would abort
// the run. Prune them at the directory level so we never descend, whether
// or not --respect-gitignore is active.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    ".godot",
    "node_modules",
    "__pycache__",
    "venv",
    "env",
    ".venv",
    "target",
    "build",
    "dist",
    ".slop-out",
];
const SKIP_EXTS: &[&str] = &["import", "uid", "md5"];

/// Whether directory walks should skip files/folders matched by the repo's
/// `.gitignore`. The CLI flag takes precedence; otherwise falls back to the
/// persistent `respect_gitignore` config setting.
pub fn should_respect_gitignore(args_respect_gitignore: bool, config: &Config) -> bool {
    args_respect_gitignore || config.respect_gitignore
}

pub struct ExclusionMatcher {
    patterns: Vec<ExclusionPattern>,
}

#[derive(Debug)]
enum ExclusionPattern {
    Glob(String),   // File pattern like "*.swift"
    Folder(String), // Folder name like "folder2"
    Regex(Regex),   // Regular expression
}

impl ExclusionMatcher {
    pub fn new(patterns: &[String]) -> Self {
        let mut matchers = Vec::new();
        for pattern in patterns {
            matchers.push(Self::compile_pattern(pattern));
        }
        ExclusionMatcher { patterns: matchers }
    }

    fn compile_pattern(pattern: &str) -> ExclusionPattern {
        // Check if it's a regex pattern (starts with / and ends with /)
        if pattern.len() >= 2 && pattern.starts_with('/') && pattern.ends_with('/') {
            let regex_str = &pattern[1..pattern.len() - 1];
            match Regex::new(regex_str) {
                Ok(re) => ExclusionPattern::Regex(re),
                Err(_) => ExclusionPattern::Glob(pattern.to_string()),
            }
        } else if pattern.ends_with('/') {
            // Folder name pattern (ends with /)
            let folder_name = pattern.trim_end_matches('/');
            ExclusionPattern::Folder(folder_name.to_string())
        } else if pattern.contains('*') || pattern.contains('?') || pattern.contains('[') {
            // Glob pattern (contains wildcards)
            ExclusionPattern::Glob(pattern.to_string())
        } else {
            // Simple pattern - could be a folder name or filename
            // Check if it looks like a folder name (no extension, common folder patterns)
            // For now, treat as glob but also check directory components
            ExclusionPattern::Glob(pattern.to_string())
        }
    }

    pub fn should_exclude(&self, path: &Path) -> bool {
        let filename = path.file_name().and_then(|f| f.to_str()).unwrap_or("");

        for pattern in &self.patterns {
            match pattern {
                ExclusionPattern::Glob(glob_pattern) => {
                    if glob_pattern.contains('*') {
                        // Convert glob pattern to regex
                        let regex_pattern = glob_to_regex(glob_pattern);
                        if let Ok(re) = Regex::new(&regex_pattern) {
                            if re.is_match(filename) {
                                return true;
                            }
                        }
                    } else {
                        // Exact match against filename
                        if filename == glob_pattern {
                            return true;
                        }
                        // Also check if it matches any directory component (for folder names)
                        if path
                            .components()
                            .any(|c| c.as_os_str().to_string_lossy() == *glob_pattern)
                        {
                            return true;
                        }
                    }
                }
                ExclusionPattern::Folder(folder_name) => {
                    // Check if any component in the path matches the folder name
                    if path
                        .components()
                        .any(|c| c.as_os_str().to_string_lossy() == *folder_name)
                    {
                        return true;
                    }
                }
                ExclusionPattern::Regex(re) => {
                    if re.is_match(filename) {
                        return true;
                    }
                }
            }
        }
        false
    }
}

fn glob_to_regex(glob: &str) -> String {
    let mut regex = String::new();
    regex.push('^');
    for c in glob.chars() {
        match c {
            '*' => regex.push_str(".*"),
            '?' => regex.push_str("."),
            '[' => regex.push_str("["),
            ']' => regex.push_str("]"),
            '.' => regex.push_str("\\."),
            c => regex.push(c),
        }
    }
    regex.push('$');
    regex
}

pub fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    if let Some(rest) = s
        .strip_prefix("$HOME/")
        .or_else(|| s.strip_prefix("${HOME}/"))
    {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

pub fn resolve_absolute(input: &Path, cwd: &Path) -> Result<PathBuf, SlopError> {
    let expanded = expand_tilde(input);
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };

    Ok(normalize_path(&joined))
}

pub fn resolve_output_dir(output_dir: Option<&Path>, cwd: &Path) -> Result<PathBuf, SlopError> {
    match output_dir {
        Some(path) => resolve_absolute(path, cwd),
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or(SlopError::HomeDirectoryResolutionFailure)?;
            Ok(normalize_path(&home.join(".slop").join("slopified")))
        }
    }
}

pub fn collect_source_files(
    inputs: &[PathBuf],
    max_depth: Option<usize>,
    exclude: &[String],
    respect_gitignore: bool,
) -> Result<Vec<PathBuf>, SlopError> {
    Ok(collect_source_files_reporting(inputs, max_depth, exclude, respect_gitignore)?.files)
}

/// Like [`collect_source_files`], but also reports what `.slopignore` pruned
/// so `--verbose` can explain the absences.
pub fn collect_source_files_reporting(
    inputs: &[PathBuf],
    max_depth: Option<usize>,
    exclude: &[String],
    respect_gitignore: bool,
) -> Result<WalkReport, SlopError> {
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    let mut forced_seen = BTreeSet::new();
    let mut forced_files = Vec::new();
    let ignored: IgnoredSink = Arc::new(Mutex::new(Vec::new()));
    let exclusion_matcher = ExclusionMatcher::new(exclude);

    for input in inputs {
        let depth = max_depth.unwrap_or(0);
        // Discovered per input: two inputs can sit in different repos, each
        // with its own .slopignore.
        let slopignore = Arc::new(SlopIgnore::discover(input));
        collect_path(
            input,
            &mut seen,
            &mut files,
            depth,
            &exclusion_matcher,
            respect_gitignore,
            &slopignore,
            &ignored,
        )?;
        collect_includes(
            &slopignore,
            &mut seen,
            &mut files,
            &mut forced_seen,
            &mut forced_files,
        )?;
    }

    files.sort_by(compare_paths_for_output);
    forced_files.sort_by(compare_paths_for_output);

    let mut ignored = ignored
        .lock()
        .map(|entries| entries.clone())
        .unwrap_or_default();
    ignored.sort_by(|left, right| compare_paths_for_output(&left.path, &right.path));
    ignored.dedup_by(|left, right| left.path == right.path);

    Ok(WalkReport {
        files,
        forced_files,
        ignored,
    })
}

/// Add all files matched by the active `.slopignore`'s include directives.
/// This runs separately from the ordinary walk so a matching file can be
/// rescued from a pruned directory, shallow traversal, or `.gitignore`.
fn collect_includes(
    slopignore: &SlopIgnore,
    seen: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
    forced_seen: &mut BTreeSet<PathBuf>,
    forced_files: &mut Vec<PathBuf>,
) -> Result<(), SlopError> {
    for path in slopignore.explicit_includes() {
        include_file(path, seen, files, forced_seen, forced_files)?;
    }

    let Some(root) = slopignore.include_root() else {
        return Ok(());
    };
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| {
            let path = error
                .path()
                .map(PathBuf::from)
                .unwrap_or_else(|| root.to_path_buf());
            SlopError::FileReadFailure {
                path,
                source: std::io::Error::other(error.to_string()),
            }
        })?;
        if entry.file_type().is_file() && slopignore.is_included(entry.path(), false) {
            include_file(entry.path(), seen, files, forced_seen, forced_files)?;
        }
    }
    Ok(())
}

fn include_file(
    path: &Path,
    seen: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
    forced_seen: &mut BTreeSet<PathBuf>,
    forced_files: &mut Vec<PathBuf>,
) -> Result<(), SlopError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(SlopError::FileReadFailure {
                path: path.to_path_buf(),
                source: error,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(());
    }
    if !is_plaintext(path) {
        eprintln!("warning: skipping non-text file: {}", path.display());
        return Ok(());
    }

    let path = equivalent_seen_path(seen, path)
        .cloned()
        .unwrap_or_else(|| path.to_path_buf());
    if insert_unique_path(forced_seen, &path) {
        forced_files.push(path.clone());
    }
    if insert_unique_path(seen, &path) {
        files.push(path);
    }
    Ok(())
}

/// Insert a path unless a different spelling already refers to the same file.
/// macOS exposes temporary files through both `/var` and `/private/var`; local
/// include scans can otherwise duplicate an explicitly named file.
fn insert_unique_path(seen: &mut BTreeSet<PathBuf>, path: &Path) -> bool {
    if seen.contains(path) {
        return false;
    }
    let canonical = path.canonicalize().ok();
    if canonical.is_some()
        && seen
            .iter()
            .filter_map(|existing| existing.canonicalize().ok())
            .any(|existing| Some(existing) == canonical)
    {
        return false;
    }
    seen.insert(path.to_path_buf())
}

fn equivalent_seen_path<'a>(seen: &'a BTreeSet<PathBuf>, path: &Path) -> Option<&'a PathBuf> {
    seen.get(path).or_else(|| {
        let canonical = path.canonicalize().ok()?;
        seen.iter()
            .find(|existing| existing.canonicalize().ok().as_ref() == Some(&canonical))
    })
}

fn record_ignored(sink: &IgnoredSink, path: &Path, is_dir: bool, reason: IgnoreReason) {
    if let Ok(mut entries) = sink.lock() {
        entries.push(IgnoredEntry {
            path: path.to_path_buf(),
            is_dir,
            reason,
        });
    }
}

pub fn build_output_filename(files: &[PathBuf], has_graph: bool) -> Result<String, SlopError> {
    if files.is_empty() {
        return Err(SlopError::InputExpandedToZeroFiles);
    }

    let mut sorted = files.to_vec();
    sorted.sort_by(compare_paths_for_output);

    let mut parts = Vec::with_capacity(sorted.len());
    for path in sorted {
        parts.push(filename_token(&path)?);
    }

    let joined = parts.join("_");
    let suffix = if has_graph { "_graph" } else { "" };
    let max_filename_len = 200;

    if joined.len() + suffix.len() + 3 <= 255 {
        Ok(format!("{}{}.md", joined, suffix))
    } else {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        joined.hash(&mut hasher);
        let hash = hasher.finish();
        let truncated = joined.chars().take(max_filename_len).collect::<String>();
        Ok(format!(
            "{}{}_{}.md",
            truncated,
            suffix,
            format!("{:x}", hash)
        ))
    }
}

pub fn filename_token(path: &Path) -> Result<String, SlopError> {
    let basename = path.file_name().ok_or_else(|| {
        SlopError::InvalidCliUsage(format!("path has no basename: {}", path.display()))
    })?;

    let basename = basename.to_string_lossy();
    let without_leading_dots = basename.trim_start_matches('.');

    if without_leading_dots.is_empty() {
        return Ok("file".to_string());
    }

    let token = match without_leading_dots.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => without_leading_dots,
    };

    if token.is_empty() {
        Ok("file".to_string())
    } else {
        Ok(token.to_string())
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_path(
    input: &Path,
    seen: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
    max_depth: usize,
    exclusion_matcher: &ExclusionMatcher,
    respect_gitignore: bool,
    slopignore: &Arc<SlopIgnore>,
    ignored: &IgnoredSink,
) -> Result<(), SlopError> {
    let metadata = fs::symlink_metadata(input).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SlopError::MissingInputPath(input.to_path_buf())
        } else {
            SlopError::FileReadFailure {
                path: input.to_path_buf(),
                source: error,
            }
        }
    })?;

    let file_type = metadata.file_type();
    if file_type.is_symlink() || !is_supported_file_type(&file_type) {
        return Err(SlopError::UnsupportedFileType(input.to_path_buf()));
    }

    // A file named explicitly on the command line is always slopped:
    // .slopignore governs directory walks, not deliberate requests.
    if metadata.is_file() {
        if !exclusion_matcher.should_exclude(input) && insert_unique_path(seen, input) {
            files.push(input.to_path_buf());
        }
        return Ok(());
    }

    // If it's a directory, traverse it with the specified depth
    if metadata.is_dir() {
        if respect_gitignore {
            return collect_dir_respecting_gitignore(
                input,
                seen,
                files,
                max_depth,
                exclusion_matcher,
                slopignore,
                ignored,
            );
        }
        return collect_dir(
            input,
            seen,
            files,
            max_depth,
            exclusion_matcher,
            slopignore,
            ignored,
        );
    }

    Err(SlopError::UnsupportedFileType(input.to_path_buf()))
}

/// Whether `input` is the process's current working directory. Shallow mode
/// (max_depth == 0) allows one extra level of depth when sloping "." so that
/// files inside immediate subdirectories are still picked up.
fn is_current_dir(input: &Path) -> bool {
    let cwd = std::env::current_dir().ok();
    let resolved = std::fs::canonicalize(input).ok();
    cwd.as_ref()
        .and_then(|c| resolved.as_ref().map(|r| c == r))
        .unwrap_or(false)
}

fn max_allowed_depth(max_depth: usize, input: &Path) -> usize {
    // For shallow mode (max_depth=0):
    // - If input is current directory (like "."), we want depth <= 2 (files in current dir + immediate children of subdirs)
    // - If input is a subdirectory, we want depth <= 1 (files directly in this dir only)
    // For full recursion (max_depth=usize::MAX), we want everything
    if max_depth == 0 {
        if is_current_dir(input) { 2 } else { 1 }
    } else {
        max_depth
    }
}

/// Walk up from `input` (inclusive) looking for the nearest ancestor that
/// owns a `.git` marker — i.e. the root of the containing git project.
/// Returns `None` when `input` is not inside any git repository at all.
///
/// Both a `.git` directory and a `.git` file (gitlink/submodule) count, so
/// this also works for worktrees and submodules. Symlinks are not followed
/// on the climb, matching how `ignore::WalkBuilder` resolves parent paths.
pub(crate) fn containing_git_root(input: &Path) -> Option<PathBuf> {
    let start = input.canonicalize().unwrap_or_else(|_| input.to_path_buf());
    let mut current: &Path = &start;
    loop {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }
}

/// Default directory walk: no `.gitignore` awareness, just the hardcoded
/// `SKIP_DIRS`/`SKIP_EXTS` prunes plus whatever `-x/--exclude` supplies.
fn collect_dir(
    input: &Path,
    seen: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
    max_depth: usize,
    exclusion_matcher: &ExclusionMatcher,
    slopignore: &Arc<SlopIgnore>,
    ignored: &IgnoredSink,
) -> Result<(), SlopError> {
    // Use WalkDir but limit depth if max_depth is 0 (shallow) or usize::MAX (full)
    // max_depth of 0 means immediate children only (depth 1 from input)
    // max_depth of usize::MAX means full recursion
    let base_depth = input.components().count();
    let filter_slopignore = Arc::clone(slopignore);
    let filter_ignored = Arc::clone(ignored);
    let walker = WalkDir::new(input)
        .follow_links(false)
        .into_iter()
        .filter_entry(move |entry| {
            if entry.depth() == 0 {
                return true;
            }
            let is_dir = entry.file_type().is_dir();
            if is_dir {
                let name = entry.file_name().to_string_lossy();
                if SKIP_DIRS.contains(&name.as_ref()) {
                    return false;
                }
            }
            if filter_slopignore.is_ignored(entry.path(), is_dir) {
                record_ignored(
                    &filter_ignored,
                    entry.path(),
                    is_dir,
                    IgnoreReason::SlopIgnore,
                );
                return false;
            }
            true
        });

    let max_allowed = max_allowed_depth(max_depth, input);

    for entry in walker {
        let entry = entry.map_err(|error| {
            let path = error
                .path()
                .map(PathBuf::from)
                .unwrap_or_else(|| input.to_path_buf());
            SlopError::FileReadFailure {
                path,
                source: std::io::Error::other(error.to_string()),
            }
        })?;

        let entry_path = entry.path();
        // Calculate depth relative to input (1 = immediate child, 2 = grandchild, etc.)
        let entry_depth = entry_path.components().count() - base_depth;

        // Skip if deeper than max_allowed_depth
        if entry_depth > max_allowed {
            continue;
        }

        let entry_metadata =
            fs::symlink_metadata(entry_path).map_err(|error| SlopError::FileReadFailure {
                path: entry_path.to_path_buf(),
                source: error,
            })?;

        let entry_type = entry_metadata.file_type();
        if entry_type.is_symlink() {
            continue;
        }

        if !is_supported_file_type(&entry_type) {
            return Err(SlopError::UnsupportedFileType(entry_path.to_path_buf()));
        }

        if entry_metadata.is_file() {
            if exclusion_matcher.should_exclude(entry_path) {
                record_ignored(ignored, entry_path, false, IgnoreReason::Exclude);
                continue;
            }
            if let Some(ext) = entry_path.extension().and_then(|e| e.to_str()) {
                if SKIP_EXTS.contains(&ext) {
                    continue;
                }
            }
            if !is_plaintext(entry_path) {
                eprintln!("warning: skipping non-text file: {}", entry_path.display());
                continue;
            }
            if insert_unique_path(seen, entry_path) {
                files.push(entry_path.to_path_buf());
            }
        }
    }
    Ok(())
}

/// `--respect-gitignore` directory walk: identical shape to `collect_dir`,
/// but sourced from `ignore::WalkBuilder` so that any `.gitignore` file
/// found at or below `input` (including nested ones, with full negation
/// support) prunes matching files and folders.
///
/// When `input` lives inside a containing git repository, the walker also
/// climbs the directory hierarchy to consult `.gitignore` files in ancestor
/// directories, stopping at the first ancestor that contains a `.git`
/// directory (the "containing git project"). This matters when `input` is a
/// subdirectory of a repo: a `.gitignore` at the repo root should still prune
/// files under `input`. The climb is bounded by `require_git(true)`, so
/// `.gitignore` files belonging to an *outer* repo (above the nearest `.git`)
/// are never consulted — mirroring how `git` itself scopes ignore rules.
///
/// When `input` is not inside any git repository, the walker falls back to
/// honoring only `.gitignore` files at or below `input` (the historical
/// behavior), so a lone `.gitignore` in a non-git directory still prunes.
///
/// In both modes the scope is otherwise tight: global git config,
/// `.git/info/exclude`, and plain `.ignore` files are disabled, so behavior
/// only ever depends on actual `.gitignore` content.
fn collect_dir_respecting_gitignore(
    input: &Path,
    seen: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
    max_depth: usize,
    exclusion_matcher: &ExclusionMatcher,
    slopignore: &Arc<SlopIgnore>,
    ignored: &IgnoredSink,
) -> Result<(), SlopError> {
    let max_allowed = max_allowed_depth(max_depth, input);

    // Walk up from `input` looking for the nearest ancestor (inclusive) that
    // owns a `.git` marker. If found, `input` is inside a real git repo, so
    // we enable parent climbing and require a `.git` to bound it. If not
    // found, we keep the legacy mode that honors a standalone `.gitignore`
    // even without a repo.
    let inside_git_repo = containing_git_root(input).is_some();

    let mut builder = WalkBuilder::new(input);
    builder
        .follow_links(false)
        .hidden(false) // slop has always included dotfiles; only .gitignore rules should newly exclude anything
        .parents(inside_git_repo) // climb ancestors for .gitignore only when bounded by a real repo
        .ignore(false) // plain .ignore files are a ripgrep convention, not requested here
        .git_ignore(true)
        .git_global(false) // scope strictly to the repo's own .gitignore, not the user's machine-wide config
        .git_exclude(false) // scope strictly to .gitignore, not .git/info/exclude
        .require_git(inside_git_repo) // bound the parent climb at the nearest `.git`
        .filter_entry({
            let filter_slopignore = Arc::clone(slopignore);
            let filter_ignored = Arc::clone(ignored);
            move |entry| {
                if entry.depth() == 0 {
                    return true;
                }
                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                if is_dir {
                    let name = entry.file_name().to_string_lossy();
                    if SKIP_DIRS.contains(&name.as_ref()) {
                        return false;
                    }
                }
                if filter_slopignore.is_ignored(entry.path(), is_dir) {
                    record_ignored(
                        &filter_ignored,
                        entry.path(),
                        is_dir,
                        IgnoreReason::SlopIgnore,
                    );
                    return false;
                }
                true
            }
        });

    for entry in builder.build() {
        let entry = entry.map_err(|error| SlopError::FileReadFailure {
            path: input.to_path_buf(),
            source: std::io::Error::other(error.to_string()),
        })?;

        let entry_depth = entry.depth();
        if entry_depth > max_allowed {
            continue;
        }

        let entry_path = entry.path();
        let entry_metadata =
            fs::symlink_metadata(entry_path).map_err(|error| SlopError::FileReadFailure {
                path: entry_path.to_path_buf(),
                source: error,
            })?;

        let entry_type = entry_metadata.file_type();
        if entry_type.is_symlink() {
            continue;
        }

        if !is_supported_file_type(&entry_type) {
            return Err(SlopError::UnsupportedFileType(entry_path.to_path_buf()));
        }

        if entry_metadata.is_file() {
            if exclusion_matcher.should_exclude(entry_path) {
                record_ignored(ignored, entry_path, false, IgnoreReason::Exclude);
                continue;
            }
            if let Some(ext) = entry_path.extension().and_then(|e| e.to_str()) {
                if SKIP_EXTS.contains(&ext) {
                    continue;
                }
            }
            if !is_plaintext(entry_path) {
                eprintln!("warning: skipping non-text file: {}", entry_path.display());
                continue;
            }
            if insert_unique_path(seen, entry_path) {
                files.push(entry_path.to_path_buf());
            }
        }
    }
    Ok(())
}

/// Heuristic check used while walking a directory: a file is treated as
/// non-text (and skipped from the slop) if its leading bytes contain a NUL
/// byte. This excludes binary artifacts such as `.DS_Store`, images, and
/// audio that would otherwise abort the run, while leaving explicit
/// single-file inputs untouched (those still surface a hard
/// `Utf8DecodeFailure`). Text files never contain NUL bytes, so this avoids
/// false positives on valid UTF-8 that happens to split a multibyte
/// sequence at the read boundary.
///
/// Also detects Godot resource files (`.tscn`/`.tres`) that contain
/// embedded `PackedByteArray(...)` texture data — these are technically
/// UTF-8 text but hold megabytes of comma-separated integer pixel data
/// that bloats the slop without value.
fn is_plaintext(path: &Path) -> bool {
    use std::io::Read;

    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return true,
    };

    let mut buf = [0u8; 8192];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return true,
    };

    if buf[..n].contains(&0) {
        return false;
    }

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if (ext == "tscn" || ext == "tres") && has_packed_byte_array(&buf[..n]) {
            return false;
        }
    }

    true
}

/// Checks whether a byte buffer contains the `PackedByteArray(` marker,
/// indicating an embedded binary blob in a Godot resource file.
fn has_packed_byte_array(bytes: &[u8]) -> bool {
    const MARKER: &[u8] = b"PackedByteArray(";
    if bytes.len() < MARKER.len() {
        return false;
    }
    bytes.windows(MARKER.len()).any(|window| window == MARKER)
}

fn compare_paths_for_output(left: &PathBuf, right: &PathBuf) -> std::cmp::Ordering {
    let left_token = filename_token(left).unwrap_or_else(|_| left.display().to_string());
    let right_token = filename_token(right).unwrap_or_else(|_| right.display().to_string());

    left_token.cmp(&right_token).then_with(|| left.cmp(right))
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    normalized
}

fn is_supported_file_type(file_type: &fs::FileType) -> bool {
    if file_type.is_file() || file_type.is_dir() {
        return true;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        if file_type.is_fifo()
            || file_type.is_socket()
            || file_type.is_block_device()
            || file_type.is_char_device()
        {
            return false;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::{
        build_output_filename, collect_source_files, collect_source_files_reporting,
        filename_token, resolve_absolute, should_respect_gitignore,
    };
    use crate::config::Config;
    use crate::models::IgnoreReason;

    #[test]
    fn resolves_relative_inputs_to_absolute_paths() {
        let cwd = PathBuf::from("/tmp/example");
        let resolved = resolve_absolute(PathBuf::from("nested/file.txt").as_path(), &cwd)
            .expect("path should resolve");
        assert_eq!(resolved, PathBuf::from("/tmp/example/nested/file.txt"));
    }

    #[test]
    fn recursively_collects_nested_files_and_hidden_files() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("nested/deeper")).expect("directories should be created");
        fs::write(root.join("visible.txt"), "hello").expect("file should be written");
        fs::write(root.join(".hidden"), "secret").expect("file should be written");
        fs::write(root.join("nested/deeper/file.md"), "nested").expect("file should be written");

        let files = collect_source_files(std::slice::from_ref(&root), Some(usize::MAX), &[], false)
            .expect("files should collect");

        assert_eq!(files.len(), 3);
        assert!(files.contains(&root.join("visible.txt")));
        assert!(files.contains(&root.join(".hidden")));
        assert!(files.contains(&root.join("nested/deeper/file.md")));
    }

    #[test]
    fn deduplicates_repeated_inputs() {
        let temp = tempdir().expect("tempdir should exist");
        let dir = temp.path().join("root");
        fs::create_dir_all(&dir).expect("directory should be created");
        let file = dir.join("alpha.txt");
        fs::write(&file, "alpha").expect("file should be written");

        let files = collect_source_files(&[file.clone(), dir], Some(usize::MAX), &[], false)
            .expect("files should collect");
        assert_eq!(files, vec![file]);
    }

    #[test]
    fn slopifies_directory_without_recursive_flag() {
        let temp = tempdir().expect("tempdir should exist");
        let dir = temp.path().join("root");
        fs::create_dir_all(&dir).expect("directory should be created");
        fs::create_dir_all(dir.join("subdir")).expect("subdir should be created");
        fs::write(dir.join("file.txt"), "content").expect("file should be written");
        fs::write(dir.join("subdir/nested.txt"), "nested").expect("file should be written");

        let files = collect_source_files(&[dir.clone()], Some(0), &[], false)
            .expect("files should collect");
        assert_eq!(files.len(), 1);
        assert!(files.contains(&dir.join("file.txt")));
        assert!(!files.iter().any(|p| p.ends_with("nested.txt")));
    }

    #[test]
    fn slopifies_only_direct_files_without_recursive_flag() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("subdir")).expect("directories should be created");
        let file1 = root.join("file1.txt");
        let file2 = root.join("file2.txt");
        fs::write(&file1, "content1").expect("file should be written");
        fs::write(&file2, "content2").expect("file should be written");
        fs::write(root.join("subdir/nested.txt"), "nested").expect("file should be written");

        let files = collect_source_files(&[file1.clone(), file2.clone()], Some(0), &[], false)
            .expect("files should collect");
        assert_eq!(files.len(), 2);
        assert!(files.contains(&file1));
        assert!(files.contains(&file2));
        assert!(!files.iter().any(|p| p.ends_with("nested.txt")));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_unsupported_file_types() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let temp = tempdir().expect("tempdir should exist");
        let fifo = temp.path().join("named_pipe");
        let fifo_c = CString::new(fifo.as_os_str().as_bytes()).expect("valid c string");

        let result = unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o644) };
        assert_eq!(result, 0, "mkfifo should succeed");

        let error = collect_source_files(&[fifo], Some(0), &[], false)
            .expect_err("fifo should be rejected");
        assert!(error.to_string().contains("unsupported file type"));
    }

    #[test]
    fn generates_filename_tokens_correctly() {
        assert_eq!(
            filename_token(PathBuf::from("/tmp/file1.md").as_path()).unwrap(),
            "file1"
        );
        assert_eq!(
            filename_token(PathBuf::from("/tmp/.env").as_path()).unwrap(),
            "env"
        );
        assert_eq!(
            filename_token(PathBuf::from("/tmp/.gitignore").as_path()).unwrap(),
            "gitignore"
        );
    }

    #[test]
    fn orders_files_deterministically_and_builds_filename() {
        let files = vec![
            PathBuf::from("/tmp/zeta/file4.md"),
            PathBuf::from("/tmp/alpha/file2.md"),
            PathBuf::from("/tmp/alpha/file1.md"),
            PathBuf::from("/tmp/beta/file3.md"),
        ];

        let filename = build_output_filename(&files, false).expect("filename should build");
        assert_eq!(filename, "file1_file2_file3_file4.md");

        let filename_graph = build_output_filename(&files, true).expect("filename should build");
        assert_eq!(filename_graph, "file1_file2_file3_file4_graph.md");
    }

    #[test]
    fn should_respect_gitignore_respects_flag() {
        let config = Config::default();
        assert!(should_respect_gitignore(true, &config));
        assert!(!should_respect_gitignore(false, &config));
    }

    #[test]
    fn should_respect_gitignore_respects_config() {
        let mut config = Config::default();
        config.respect_gitignore = true;
        assert!(should_respect_gitignore(false, &config));
    }

    #[test]
    fn does_not_respect_gitignore_by_default() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        fs::create_dir_all(&root).expect("directory should be created");
        fs::write(root.join(".gitignore"), "ignored.txt\n").expect("gitignore should be written");
        fs::write(root.join("ignored.txt"), "skip").expect("file should be written");

        let files = collect_source_files(&[root.clone()], Some(usize::MAX), &[], false)
            .expect("files should collect");

        // Without the flag, .gitignore is just another file - it prunes nothing.
        assert!(files.iter().any(|p| p.ends_with("ignored.txt")));
    }

    #[test]
    fn respects_gitignore_when_flag_enabled() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        fs::create_dir_all(&root).expect("directory should be created");
        fs::write(root.join(".gitignore"), "ignored.txt\n").expect("gitignore should be written");
        fs::write(root.join("visible.txt"), "keep").expect("file should be written");
        fs::write(root.join("ignored.txt"), "skip").expect("file should be written");

        let files = collect_source_files(&[root.clone()], Some(usize::MAX), &[], true)
            .expect("files should collect");

        assert!(files.contains(&root.join("visible.txt")));
        assert!(!files.iter().any(|p| p.ends_with("ignored.txt")));
        // Dotfiles are still slopified by default - only .gitignore-matched
        // paths should be newly excluded.
        assert!(files.contains(&root.join(".gitignore")));
    }

    #[test]
    fn respects_nested_gitignore_files() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("sub")).expect("directories should be created");
        fs::write(root.join("sub/.gitignore"), "secret.txt\n")
            .expect("gitignore should be written");
        fs::write(root.join("sub/secret.txt"), "skip").expect("file should be written");
        fs::write(root.join("sub/keep.txt"), "keep").expect("file should be written");

        let files = collect_source_files(&[root.clone()], Some(usize::MAX), &[], true)
            .expect("files should collect");

        assert!(files.contains(&root.join("sub/keep.txt")));
        assert!(!files.iter().any(|p| p.ends_with("secret.txt")));
    }

    #[test]
    fn respects_gitignore_directory_patterns() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        // "generated" isn't in the hardcoded SKIP_DIRS list, so this proves
        // the exclusion comes from .gitignore parsing, not the safety-net prune.
        fs::create_dir_all(root.join("generated")).expect("directories should be created");
        fs::write(root.join(".gitignore"), "generated/\n").expect("gitignore should be written");
        fs::write(root.join("generated/bundle.js"), "compiled").expect("file should be written");
        fs::write(root.join("keep.txt"), "keep").expect("file should be written");

        let files = collect_source_files(&[root.clone()], Some(usize::MAX), &[], true)
            .expect("files should collect");

        assert!(files.contains(&root.join("keep.txt")));
        assert!(!files.iter().any(|p| p.ends_with("bundle.js")));
    }

    #[test]
    fn respects_gitignore_negation_patterns() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        fs::create_dir_all(&root).expect("directory should be created");
        fs::write(root.join(".gitignore"), "*.log\n!important.log\n")
            .expect("gitignore should be written");
        fs::write(root.join("debug.log"), "noisy").expect("file should be written");
        fs::write(root.join("important.log"), "keep me").expect("file should be written");

        let files = collect_source_files(&[root.clone()], Some(usize::MAX), &[], true)
            .expect("files should collect");

        assert!(files.contains(&root.join("important.log")));
        assert!(!files.iter().any(|p| p.ends_with("debug.log")));
    }

    #[test]
    fn skip_dirs_still_pruned_with_respect_gitignore() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("node_modules")).expect("directories should be created");
        fs::write(root.join("node_modules/pkg.js"), "vendored").expect("file should be written");
        fs::write(root.join("keep.txt"), "keep").expect("file should be written");

        let files = collect_source_files(&[root.clone()], Some(usize::MAX), &[], true)
            .expect("files should collect");

        assert!(files.contains(&root.join("keep.txt")));
        assert!(!files.iter().any(|p| p.ends_with("pkg.js")));
    }

    #[test]
    fn explicit_file_arguments_bypass_gitignore() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        fs::create_dir_all(&root).expect("directory should be created");
        fs::write(root.join(".gitignore"), "secret.txt\n").expect("gitignore should be written");
        let secret = root.join("secret.txt");
        fs::write(&secret, "explicit").expect("file should be written");

        // Naming a gitignored file directly still slopifies it - only
        // directory traversal is pruned by --respect-gitignore.
        let files = collect_source_files(&[secret.clone()], Some(0), &[], true)
            .expect("files should collect");
        assert_eq!(files, vec![secret]);
    }

    #[test]
    fn respects_gitignore_from_containing_repo_root() {
        // Mirrors the field report: `input` is a *subdirectory* of a git
        // repo, and the repo's `.gitignore` lives at the repo root (an
        // ancestor of `input`). The walker must climb to the repo root and
        // apply its rules to files under `input`.
        let temp = tempdir().expect("tempdir should exist");
        let repo = temp.path().join("repo");
        fs::create_dir_all(repo.join("FEATURES/screenshots")).expect("dirs created");
        // The repo root owns `.git`, marking this as the containing project.
        fs::create_dir_all(repo.join(".git")).expect(".git created");
        fs::write(repo.join(".gitignore"), "*.png\n.DS_Store\n").expect("gitignore written");
        fs::write(repo.join("FEATURES/.DS_Store"), "noise").expect("ds written");
        fs::write(repo.join("FEATURES/keep.md"), "keep").expect("keep written");
        fs::write(repo.join("FEATURES/screenshots/Screenshot.png"), "binary").expect("png written");

        let input = repo.join("FEATURES");
        let files = collect_source_files(&[input], Some(usize::MAX), &[], true)
            .expect("files should collect");

        assert!(files.contains(&repo.join("FEATURES/keep.md")));
        assert!(
            !files.iter().any(|p| p.ends_with(".DS_Store")),
            "repo-root .gitignore should have pruned .DS_Store: {:?}",
            files
        );
        assert!(
            !files.iter().any(|p| p.ends_with("Screenshot.png")),
            "repo-root .gitignore should have pruned *.png: {:?}",
            files
        );
    }

    #[test]
    fn does_not_consult_outer_repo_gitignore_for_nested_repo() {
        // `input` is itself a git repo. An ancestor directory also holds a
        // `.gitignore` (and even a `.git`). The walker must stop at `input`'s
        // own `.git` and must NOT apply the outer repo's ignore rules to
        // `input`'s contents — mirroring how `git` scopes ignore rules to
        // the current repository.
        let temp = tempdir().expect("tempdir should exist");
        // Outer repo (the "host" filesystem) with a .gitignore that would
        // wrongly prune `keep.txt` if consulted.
        fs::create_dir_all(temp.path().join(".git")).expect("outer .git created");
        fs::write(temp.path().join(".gitignore"), "keep.txt\n").expect("outer gitignore written");

        let inner = temp.path().join("inner");
        fs::create_dir_all(&inner).expect("inner created");
        fs::create_dir_all(inner.join(".git")).expect("inner .git created");
        fs::write(inner.join(".gitignore"), "ignored.txt\n").expect("inner gitignore written");
        fs::write(inner.join("keep.txt"), "keep").expect("keep written");
        fs::write(inner.join("ignored.txt"), "skip").expect("skip written");

        let files = collect_source_files(&[inner.clone()], Some(usize::MAX), &[], true)
            .expect("files should collect");

        // Inner repo's own .gitignore is honored.
        assert!(!files.iter().any(|p| p.ends_with("ignored.txt")));
        // Outer repo's .gitignore does NOT leak into the inner repo.
        assert!(
            files.contains(&inner.join("keep.txt")),
            "outer repo .gitignore should not prune inner repo files: {:?}",
            files
        );
    }

    #[test]
    fn respects_slopignore_without_any_flag() {
        let temp = tempdir().expect("temp dir should be created");
        let root = temp.path().to_path_buf();
        fs::write(root.join(".slopignore"), "*.log\n").expect("slopignore should be written");
        fs::write(root.join("keep.rs"), "keep").expect("file should be written");
        fs::write(root.join("debug.log"), "noise").expect("file should be written");

        let report = collect_source_files_reporting(&[root.clone()], Some(usize::MAX), &[], false)
            .expect("collection should succeed");

        assert!(report.files.contains(&root.join("keep.rs")));
        assert!(
            !report.files.contains(&root.join("debug.log")),
            ".slopignore applies with no flag at all"
        );
        assert_eq!(report.ignored.len(), 1);
        assert_eq!(report.ignored[0].path, root.join("debug.log"));
        assert!(!report.ignored[0].is_dir);
        assert_eq!(report.ignored[0].reason, IgnoreReason::SlopIgnore);
    }

    #[test]
    fn slopignore_prunes_directories_without_descending() {
        let temp = tempdir().expect("temp dir should be created");
        let root = temp.path().to_path_buf();
        fs::create_dir_all(root.join("vendor/deep")).expect("dirs should be created");
        fs::write(root.join(".slopignore"), "vendor/\n").expect("slopignore should be written");
        fs::write(root.join("keep.rs"), "keep").expect("file should be written");
        fs::write(root.join("vendor/a.rs"), "skip").expect("file should be written");
        fs::write(root.join("vendor/deep/b.rs"), "skip").expect("file should be written");

        let report = collect_source_files_reporting(&[root.clone()], Some(usize::MAX), &[], false)
            .expect("collection should succeed");

        assert!(report.files.contains(&root.join("keep.rs")));
        assert!(
            !report
                .files
                .iter()
                .any(|path| path.starts_with(root.join("vendor")))
        );
        assert_eq!(
            report.ignored.len(),
            1,
            "a pruned directory is reported once, not once per descendant"
        );
        assert_eq!(report.ignored[0].path, root.join("vendor"));
        assert!(report.ignored[0].is_dir);
        assert_eq!(report.ignored[0].reason, IgnoreReason::SlopIgnore);
    }

    #[test]
    fn slopinclude_overrides_an_ignored_directory_and_walk_depth() {
        let temp = tempdir().expect("temp dir should be created");
        let root = temp.path().to_path_buf();
        let forced = root.join("generated/deep/forced.rs");
        fs::create_dir_all(forced.parent().expect("forced parent"))
            .expect("directories should be created");
        fs::write(&forced, "forced").expect("file should be written");
        fs::write(
            root.join(".slopignore"),
            "generated/\n+ generated/deep/forced.rs\n",
        )
        .expect("slopignore should be written");

        let report = collect_source_files_reporting(&[root], Some(0), &[], false)
            .expect("collection should succeed");

        assert!(
            report.files.contains(&forced),
            "an include overrides the ignored parent and shallow walk"
        );
        assert_eq!(report.forced_files, vec![forced]);
    }

    #[test]
    fn slopignore_and_gitignore_are_independent() {
        let temp = tempdir().expect("temp dir should be created");
        let root = temp.path().to_path_buf();
        fs::write(root.join(".gitignore"), "git_only.txt\n").expect("gitignore should be written");
        fs::write(root.join(".slopignore"), "slop_only.txt\n")
            .expect("slopignore should be written");
        fs::write(root.join("git_only.txt"), "a").expect("file should be written");
        fs::write(root.join("slop_only.txt"), "b").expect("file should be written");

        // Without --respect-gitignore only .slopignore prunes.
        let files = collect_source_files(&[root.clone()], Some(usize::MAX), &[], false)
            .expect("collection should succeed");
        assert!(files.contains(&root.join("git_only.txt")));
        assert!(!files.contains(&root.join("slop_only.txt")));

        // With --respect-gitignore both apply.
        let files = collect_source_files(&[root.clone()], Some(usize::MAX), &[], true)
            .expect("collection should succeed");
        assert!(!files.contains(&root.join("git_only.txt")));
        assert!(!files.contains(&root.join("slop_only.txt")));
    }

    #[test]
    fn slopignore_does_not_filter_explicit_file_inputs() {
        let temp = tempdir().expect("temp dir should be created");
        let root = temp.path().to_path_buf();
        fs::write(root.join(".slopignore"), "*.log\n").expect("slopignore should be written");
        let explicit = root.join("debug.log");
        fs::write(&explicit, "noise").expect("file should be written");

        let files = collect_source_files(&[explicit.clone()], Some(0), &[], false)
            .expect("collection should succeed");
        assert_eq!(
            files,
            vec![explicit],
            "naming a file explicitly overrides .slopignore"
        );
    }

    #[test]
    fn exclude_patterns_are_reported_with_their_own_reason() {
        let temp = tempdir().expect("temp dir should be created");
        let root = temp.path().to_path_buf();
        fs::write(root.join("keep.rs"), "keep").expect("file should be written");
        fs::write(root.join("notes.md"), "skip").expect("file should be written");

        let report = collect_source_files_reporting(
            &[root.clone()],
            Some(usize::MAX),
            &["*.md".to_string()],
            false,
        )
        .expect("collection should succeed");

        assert!(report.files.contains(&root.join("keep.rs")));
        assert!(!report.files.contains(&root.join("notes.md")));
        assert_eq!(report.ignored.len(), 1);
        assert_eq!(report.ignored[0].path, root.join("notes.md"));
        assert_eq!(report.ignored[0].reason, IgnoreReason::Exclude);
    }

    #[test]
    fn walk_report_is_empty_when_no_slopignore_exists() {
        let temp = tempdir().expect("temp dir should be created");
        let root = temp.path().to_path_buf();
        fs::write(root.join("keep.rs"), "keep").expect("file should be written");

        let report = collect_source_files_reporting(&[root], Some(usize::MAX), &[], false)
            .expect("collection should succeed");
        assert_eq!(report.files.len(), 1);
        assert!(report.ignored.is_empty());
    }
}
