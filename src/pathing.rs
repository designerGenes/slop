use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use regex::Regex;
use walkdir::WalkDir;

use crate::error::SoupifyError;

pub struct ExclusionMatcher {
    patterns: Vec<ExclusionPattern>,
}

#[derive(Debug)]
enum ExclusionPattern {
    Glob(String),        // File pattern like "*.swift"
    Folder(String),      // Folder name like "folder2"
    Regex(Regex),        // Regular expression
}

impl ExclusionMatcher {
    pub fn new(patterns: &[String]) -> Self {
        let mut matchers = Vec::new();
        for pattern in patterns {
            matchers.push(Self::compile_pattern(pattern));
        }
        ExclusionMatcher {
            patterns: matchers,
        }
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
        let filename = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("");

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
    path.to_path_buf()
}

pub fn resolve_absolute(input: &Path, cwd: &Path) -> Result<PathBuf, SoupifyError> {
    let expanded = expand_tilde(input);
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    };

    Ok(normalize_path(&joined))
}

pub fn resolve_output_dir(output_dir: Option<&Path>, cwd: &Path) -> Result<PathBuf, SoupifyError> {
    match output_dir {
        Some(path) => resolve_absolute(path, cwd),
        None => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or(SoupifyError::HomeDirectoryResolutionFailure)?;
            Ok(normalize_path(&home.join(".soupify").join("soupified")))
        }
    }
}

pub fn collect_source_files(
    inputs: &[PathBuf],
    max_depth: Option<usize>,
    exclude: &[String],
) -> Result<Vec<PathBuf>, SoupifyError> {
    let mut seen = BTreeSet::new();
    let mut files = Vec::new();
    let exclusion_matcher = ExclusionMatcher::new(exclude);

    for input in inputs {
        let depth = max_depth.unwrap_or(0);
        collect_path(input, &mut seen, &mut files, depth, &exclusion_matcher)?;
    }

    files.sort_by(compare_paths_for_output);
    Ok(files)
}

pub fn build_output_filename(files: &[PathBuf], has_graph: bool) -> Result<String, SoupifyError> {
    if files.is_empty() {
        return Err(SoupifyError::InputExpandedToZeroFiles);
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
        Ok(format!("{}{}_{}.md", truncated, suffix, format!("{:x}", hash)))
    }
}

pub fn filename_token(path: &Path) -> Result<String, SoupifyError> {
    let basename = path.file_name().ok_or_else(|| {
        SoupifyError::InvalidCliUsage(format!("path has no basename: {}", path.display()))
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

fn collect_path(
    input: &Path,
    seen: &mut BTreeSet<PathBuf>,
    files: &mut Vec<PathBuf>,
    max_depth: usize,
    exclusion_matcher: &ExclusionMatcher,
) -> Result<(), SoupifyError> {
    let metadata = fs::symlink_metadata(input).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            SoupifyError::MissingInputPath(input.to_path_buf())
        } else {
            SoupifyError::FileReadFailure {
                path: input.to_path_buf(),
                source: error,
            }
        }
    })?;

    let file_type = metadata.file_type();
    if file_type.is_symlink() || !is_supported_file_type(&file_type) {
        return Err(SoupifyError::UnsupportedFileType(input.to_path_buf()));
    }

    if metadata.is_file() {
        if !exclusion_matcher.should_exclude(input) {
            if seen.insert(input.to_path_buf()) {
                files.push(input.to_path_buf());
            }
        }
        return Ok(());
    }

    // If it's a directory, traverse it with the specified depth
    if metadata.is_dir() {
        // Use WalkDir but limit depth if max_depth is 0 (shallow) or usize::MAX (full)
        // max_depth of 0 means immediate children only (depth 1 from input)
        // max_depth of usize::MAX means full recursion
        let base_depth = input.components().count();
        // VCS/build/output trees are never useful in a soup and routinely hold
        // binary blobs (git loose objects, compiled artifacts) that would abort
        // the run. Prune them at the directory level so we never descend.
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
            ".soup-out",
        ];
        const SKIP_EXTS: &[&str] = &[
            "import",
            "uid",
            "md5",
        ];
        let walker = WalkDir::new(input)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                if entry.depth() == 0 {
                    return true;
                }
                if entry.file_type().is_dir() {
                    let name = entry.file_name().to_string_lossy();
                    return !SKIP_DIRS.contains(&name.as_ref());
                }
                true
            });

        
        // Determine if this is the current directory (for shallow mode depth calculation)
        let is_current_dir = {
            let cwd = std::env::current_dir().ok();
            let resolved = std::fs::canonicalize(input).ok();
            cwd.as_ref().and_then(|c| resolved.as_ref().map(|r| c == r)).unwrap_or(false)
        };
        
        for entry in walker {
            let entry = entry.map_err(|error| {
                let path = error
                    .path()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| input.to_path_buf());
                SoupifyError::FileReadFailure {
                    path,
                    source: std::io::Error::other(error.to_string()),
                }
            })?;

            let entry_path = entry.path();
            // Calculate depth relative to input (1 = immediate child, 2 = grandchild, etc.)
            let entry_depth = entry_path.components().count() - base_depth;
            
            // For shallow mode (max_depth=0):
            // - If input is current directory (like "."), we want depth <= 2 (files in current dir + immediate children of subdirs)
            // - If input is a subdirectory, we want depth <= 1 (files directly in this dir only)
            // For full recursion (max_depth=usize::MAX), we want everything
            let max_allowed_depth = if max_depth == 0 {
                if is_current_dir { 2 } else { 1 }
            } else {
                max_depth
            };
            
            // Skip if deeper than max_allowed_depth
            if entry_depth > max_allowed_depth {
                continue;
            }

            let entry_metadata = fs::symlink_metadata(entry_path).map_err(|error| {
                SoupifyError::FileReadFailure {
                    path: entry_path.to_path_buf(),
                    source: error,
                }
            })?;

            let entry_type = entry_metadata.file_type();
            if entry_type.is_symlink() {
                continue;
            }
            
            if !is_supported_file_type(&entry_type) {
                return Err(SoupifyError::UnsupportedFileType(entry_path.to_path_buf()));
            }

            if entry_metadata.is_file() && !exclusion_matcher.should_exclude(entry_path) {
                if let Some(ext) = entry_path.extension().and_then(|e| e.to_str()) {
                    if SKIP_EXTS.contains(&ext) {
                        continue;
                    }
                }
                if !is_plaintext(entry_path) {
                    eprintln!("warning: skipping non-text file: {}", entry_path.display());
                    continue;
                }
                if seen.insert(entry_path.to_path_buf()) {
                    files.push(entry_path.to_path_buf());
                }
            }
        }
        return Ok(());
    }

    Err(SoupifyError::UnsupportedFileType(input.to_path_buf()))
}

/// Heuristic check used while walking a directory: a file is treated as
/// non-text (and skipped from the soup) if its leading bytes contain a NUL
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
/// that bloats the soup without value.
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
    bytes
        .windows(MARKER.len())
        .any(|window| window == MARKER)
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

    use super::{build_output_filename, collect_source_files, filename_token, resolve_absolute};

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

        let files =
            collect_source_files(std::slice::from_ref(&root), Some(usize::MAX), &[]).expect("files should collect");

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

        let files = collect_source_files(&[file.clone(), dir], Some(usize::MAX), &[]).expect("files should collect");
        assert_eq!(files, vec![file]);
    }

    #[test]
    fn soupifies_directory_without_recursive_flag() {
        let temp = tempdir().expect("tempdir should exist");
        let dir = temp.path().join("root");
        fs::create_dir_all(&dir).expect("directory should be created");
        fs::create_dir_all(dir.join("subdir")).expect("subdir should be created");
        fs::write(dir.join("file.txt"), "content").expect("file should be written");
        fs::write(dir.join("subdir/nested.txt"), "nested").expect("file should be written");

        let files = collect_source_files(&[dir.clone()], Some(0), &[]).expect("files should collect");
        assert_eq!(files.len(), 1);
        assert!(files.contains(&dir.join("file.txt")));
        assert!(!files.iter().any(|p| p.ends_with("nested.txt")));
    }

    #[test]
    fn soupifies_only_direct_files_without_recursive_flag() {
        let temp = tempdir().expect("tempdir should exist");
        let root = temp.path().join("root");
        fs::create_dir_all(root.join("subdir")).expect("directories should be created");
        let file1 = root.join("file1.txt");
        let file2 = root.join("file2.txt");
        fs::write(&file1, "content1").expect("file should be written");
        fs::write(&file2, "content2").expect("file should be written");
        fs::write(root.join("subdir/nested.txt"), "nested").expect("file should be written");

        let files = collect_source_files(&[file1.clone(), file2.clone()], Some(0), &[])
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

        let error = collect_source_files(&[fifo], Some(0), &[]).expect_err("fifo should be rejected");
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
}
