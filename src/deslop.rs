use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::SlopError;
use crate::models::{CliArgs, SoupBlock, SoupDocument, SoupMatchResult, SoupPartialRange};
use crate::pathing::{resolve_absolute, resolve_output_dir};
use crate::slop_format::{compute_block_id, parse_document};

pub fn find_matching_slop_file(
    selectors: &[PathBuf],
    slop_dir: &Path,
) -> Result<PathBuf, SlopError> {
    match match_slop_file(selectors, slop_dir)? {
        SoupMatchResult::One(path) => Ok(path),
        SoupMatchResult::None => Err(SlopError::NoMatchingSoupFile {
            selectors: selectors.to_vec(),
            slop_dir: slop_dir.to_path_buf(),
        }),
        SoupMatchResult::Ambiguous(paths) => Err(SlopError::AmbiguousSoupFileMatch { paths }),
    }
}

const DESLOP_CACHE_LIMIT: usize = 100;
const DESLOP_CACHE_ENV: &str = "SLOP_DESLOP_CACHE";

/// Where (and whether) the applied-block ledger lives.
///
/// Precedence: `SLOP_DESLOP_CACHE` (a path, or one of off/disabled/none/false
/// to disable) beats `deslop_cache_path` in config.yaml, which beats
/// `$HOME/.slop/.slop_blocks_cache`. The ledger is only consulted when
/// `deslop_cache: true` — deslop is idempotent without it, because a block
/// whose content already matches the file on disk is never rewritten.
fn resolve_cache_path(config: &Config) -> Option<PathBuf> {
    if let Ok(raw) = std::env::var(DESLOP_CACHE_ENV) {
        let trimmed = raw.trim();
        let disabled = trimmed.is_empty()
            || matches!(
                trimmed.to_ascii_lowercase().as_str(),
                "off" | "disabled" | "none" | "false"
            );
        if disabled {
            return None;
        }
        // Note: the path itself keeps its case. The previous implementation
        // lowercased it, which silently broke case-sensitive paths.
        return Some(crate::pathing::expand_tilde(Path::new(trimmed)));
    }

    if !config.deslop_cache {
        return None;
    }

    match config.deslop_cache_path {
        Some(ref configured) => Some(crate::pathing::expand_tilde(configured)),
        None => crate::config::default_deslop_cache_path(),
    }
}

/// Ledger key. Scoped by destination path as well as content: the same block
/// body legitimately lands at two different paths (an empty `__init__.py`, a
/// one-line `mod.rs`), and skipping the second because the first was applied
/// is silent data loss.
fn cache_key(path: &Path, block_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update(b"\0");
    hasher.update(block_id.as_bytes());
    hasher.finalize().to_hex().to_string()
}

struct DeslopCache {
    seen: HashSet<String>,
    order: Vec<String>,
    path: PathBuf,
    limit: usize,
    dirty: bool,
}

impl DeslopCache {
    fn load(path: PathBuf) -> Self {
        let order = fs::read_to_string(&path)
            .ok()
            .map(|body| {
                body.lines()
                    .map(|line| line.trim().to_string())
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let seen = order.iter().cloned().collect::<HashSet<_>>();
        Self {
            seen,
            order,
            path,
            limit: DESLOP_CACHE_LIMIT,
            dirty: false,
        }
    }

    fn contains(&self, block_id: &str) -> bool {
        self.seen.contains(block_id)
    }

    fn record(&mut self, block_id: String) {
        if self.seen.contains(&block_id) {
            return;
        }
        self.seen.insert(block_id.clone());
        self.order.push(block_id);
        while self.order.len() > self.limit {
            let evicted = self.order.remove(0);
            self.seen.remove(&evicted);
        }
        self.dirty = true;
    }

    fn persist(&mut self) {
        if !self.dirty {
            return;
        }
        if let Some((parent, Err(error))) = self
            .path
            .parent()
            .map(|parent| (parent, fs::create_dir_all(parent)))
        {
            eprintln!(
                "warning: failed to create deslop cache directory {}: {error}",
                parent.display()
            );
            return;
        }
        let body = self.order.join("\n");
        if let Err(error) = fs::write(&self.path, body) {
            eprintln!(
                "warning: failed to persist deslop cache {}: {error}",
                self.path.display()
            );
            return;
        }
        self.dirty = false;
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.order.len()
    }
}

fn block_id_for(block: &SoupBlock) -> String {
    block
        .block_id
        .clone()
        .unwrap_or_else(|| compute_block_id(&block.content_lines, block.trailing_newline))
}

fn short_id(block_id: &str) -> &str {
    let len = block_id.len().min(12);
    &block_id[..len]
}

/// Whether the file at `path` is already byte-identical to `contents`.
/// A missing or unreadable file is "not matching", so the write proceeds.
fn file_already_matches(path: &Path, contents: &str) -> bool {
    fs::read(path).is_ok_and(|existing| existing == contents.as_bytes())
}

fn warn_on_base_sha_drift(path: &Path, expected: &str) {
    let Ok(on_disk_bytes) = fs::read(path) else {
        return;
    };
    let actual = blake3::hash(&on_disk_bytes).to_hex().to_string();
    if actual != expected {
        eprintln!(
            "warning: base SHA drift for {}: expected {expected}, got {actual}; applying partial block anyway",
            path.display()
        );
    }
}

pub fn run_deslop(args: &CliArgs, config: &Config) -> Result<Vec<PathBuf>, SlopError> {
    let cwd = std::env::current_dir().map_err(|error| SlopError::FileReadFailure {
        path: PathBuf::from("."),
        source: error,
    })?;
    let slop_dir = resolve_output_dir(
        args.output_dir
            .as_deref()
            .or(args.slop_to.as_deref())
            .or(config.slopified_folder.as_deref()),
        &cwd,
    )?;
    let resolved_inputs = args
        .inputs
        .iter()
        .map(|selector| resolve_absolute(selector, &cwd))
        .collect::<Result<Vec<_>, _>>()?;

    let (_slop_file, document) = match resolve_direct_slop_document(&resolved_inputs)? {
        Some((slop_file, document)) => (slop_file, document),
        None => {
            let slop_file = find_matching_slop_file(&resolved_inputs, &slop_dir)?;
            let document = read_slop_document(&slop_file)?;
            (slop_file, document)
        }
    };

    if !document.meta_blocks.is_empty() {
        eprintln!(
            "warning: {} #SLOP_META block(s) found in slop; these are reference-only and will be skipped during deslop",
            document.meta_blocks.len()
        );
    }

    let allowed_roots = compute_allowed_roots(&document.blocks, &args.allow_roots, &cwd);

    let cache_path = if args.dry_run {
        None
    } else {
        resolve_cache_path(config)
    };
    let mut cache = cache_path.map(DeslopCache::load);

    let mut restored_paths = Vec::with_capacity(document.blocks.len());
    for block in document.blocks {
        let restored_path = block.original_absolute_path.clone();

        if block.read_only {
            eprintln!("warning: read-only block for {} skipped in deslop", restored_path.display());
            continue;
        }

        let block_id = block_id_for(&block);
        let ledger_key = cache_key(&restored_path, &block_id);
        if cache
            .as_ref()
            .is_some_and(|cache| cache.contains(&ledger_key))
        {
            eprintln!(
                "note: slop block {} for {} already deslopped; skipping (deslop_cache)",
                short_id(&block_id),
                restored_path.display()
            );
            continue;
        }

        // Drift only matters for partial blocks: a full block replaces the
        // file wholesale, so the prior contents are irrelevant.
        let drift_sha = block
            .base_sha
            .as_deref()
            .filter(|_| block.partial_range.is_some());
        if let Some(sha) = drift_sha {
            warn_on_base_sha_drift(&restored_path, sha);
        }

        if !is_within_allowed_roots(&restored_path, &allowed_roots) {
            return Err(SlopError::WriteOutsideAllowedRoot {
                path: restored_path.clone(),
                allowed_roots: allowed_roots.clone(),
            });
        }

        if block.base_sha.is_none() && !block.read_only {
            eprintln!(
                "warning: novel file in returned slop: {} (no base SHA)",
                restored_path.display()
            );
        }

        let contents = materialize_block_contents(&restored_path, &block)?;

        if args.dry_run {
            let existing = fs::read_to_string(&restored_path).unwrap_or_default();
            let diff = unified_diff(&existing, &contents, &restored_path);
            if !diff.is_empty() {
                println!("{}", diff);
            }
            restored_paths.push(restored_path);
            continue;
        }

        // The load-bearing idempotency check: if the file on disk is already
        // byte-identical to what this block produces, applying it again is a
        // no-op, so don't touch the file (and don't churn its mtime). This is
        // exact, needs no persisted state, and generalizes the check
        // `apply_partial_block` already performs for partial blocks.
        if file_already_matches(&restored_path, &contents) {
            eprintln!(
                "note: {} already matches slop block {}; leaving it untouched",
                restored_path.display(),
                short_id(&block_id)
            );
            if let Some(ref mut cache) = cache {
                cache.record(ledger_key);
            }
            restored_paths.push(restored_path);
            continue;
        }

        if let Some(parent) = restored_path.parent() {
            fs::create_dir_all(parent).map_err(|error| SlopError::DirectoryCreationFailure {
                path: parent.to_path_buf(),
                source: error,
            })?;
        }

        fs::write(&restored_path, contents).map_err(|error| SlopError::FileWriteFailure {
            path: restored_path.clone(),
            source: error,
        })?;

        if let Some(ref mut cache) = cache {
            cache.record(ledger_key);
        }
        restored_paths.push(restored_path);
    }

    if let Some(ref mut cache) = cache {
        cache.persist();
    }

    if args.dry_run {
        println!("dry-run: {} files would be written", restored_paths.len());
    }

    Ok(restored_paths)
}

fn compute_allowed_roots(blocks: &[SoupBlock], extra: &[PathBuf], cwd: &Path) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = extra
        .iter()
        .map(|p| resolve_absolute(p, cwd).unwrap_or_else(|_| p.clone()))
        .collect();

    if roots.is_empty() {
        let mut common: Option<PathBuf> = None;
        for block in blocks {
            let parent = block.original_absolute_path.parent().unwrap_or(Path::new("/"));
            common = Some(match common {
                None => parent.to_path_buf(),
                Some(ref c) => common_ancestor(c, parent),
            });
        }
        if let Some(c) = common {
            roots.push(c);
        } else {
            roots.push(cwd.to_path_buf());
        }
    }

    roots
}

fn common_ancestor(a: &Path, b: &Path) -> PathBuf {
    let a_comps: Vec<_> = a.components().collect();
    let b_comps: Vec<_> = b.components().collect();
    let mut result = PathBuf::new();
    for i in 0..a_comps.len().min(b_comps.len()) {
        if a_comps[i] == b_comps[i] {
            result.push(a_comps[i]);
        } else {
            break;
        }
    }
    if result.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        result
    }
}

fn is_within_allowed_roots(path: &Path, roots: &[PathBuf]) -> bool {
    let normalized = crate::pathing::normalize_path(path);
    for root in roots {
        let root_normalized = crate::pathing::normalize_path(root);
        if normalized.starts_with(&root_normalized) {
            return true;
        }
    }
    false
}

fn unified_diff(old: &str, new: &str, path: &Path) -> String {
    use similar::TextDiff;
    let diff = TextDiff::from_lines(old, new);
    let mut result = String::new();
    for change in diff.iter_all_changes() {
        let prefix = match change.tag() {
            similar::ChangeTag::Delete => "-",
            similar::ChangeTag::Insert => "+",
            similar::ChangeTag::Equal => " ",
        };
        result.push_str(&format!("{}{}", prefix, change));
    }
    if result.trim().is_empty() {
        return String::new();
    }
    format!("--- {} (current)\n+++ {} (slop)\n{}", path.display(), path.display(), result)
}

fn resolve_direct_slop_document(
    inputs: &[PathBuf],
) -> Result<Option<(PathBuf, SoupDocument)>, SlopError> {
    let [input] = inputs else {
        return Ok(None);
    };

    if !input.is_file() || !looks_like_slop_file(input) {
        return Ok(None);
    }

    read_slop_document(input).map(|document| Some((input.clone(), document)))
}

fn read_slop_document(path: &Path) -> Result<SoupDocument, SlopError> {
    let markdown = fs::read_to_string(path).map_err(|error| SlopError::FileReadFailure {
        path: path.to_path_buf(),
        source: error,
    })?;
    parse_document(&markdown)
}

fn looks_like_slop_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    contents.lines().next().map_or(false, |first_line| {
        first_line.starts_with("#SLOP ")
            || first_line.starts_with("#SLOP_META ")
            || first_line.starts_with("#SLOP_AUTO_UNslop")
    })
}

fn match_slop_file(
    selectors: &[PathBuf],
    slop_dir: &Path,
) -> Result<SoupMatchResult, SlopError> {
    let candidates = collect_candidate_slop_files(slop_dir)?;
    let mut matches = Vec::new();

    for candidate in candidates {
        let markdown =
            fs::read_to_string(&candidate).map_err(|error| SlopError::FileReadFailure {
                path: candidate.clone(),
                source: error,
            })?;
        let document = parse_document(&markdown)?;
        if document_matches(selectors, &document) {
            matches.push(candidate);
        }
    }

    Ok(match matches.len() {
        0 => SoupMatchResult::None,
        1 => SoupMatchResult::One(matches.remove(0)),
        _ => SoupMatchResult::Ambiguous(matches),
    })
}

fn collect_candidate_slop_files(slop_dir: &Path) -> Result<Vec<PathBuf>, SlopError> {
    let directory_entries = match fs::read_dir(slop_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(SlopError::FileReadFailure {
                path: slop_dir.to_path_buf(),
                source: error,
            });
        }
    };

    let mut files = Vec::new();
    for entry in directory_entries {
        let entry = entry.map_err(|error| SlopError::FileReadFailure {
            path: slop_dir.to_path_buf(),
            source: error,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("md") && path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn document_matches(selectors: &[PathBuf], document: &SoupDocument) -> bool {
    if selectors.is_empty() || document.blocks.is_empty() {
        return false;
    }

    let mut covered = BTreeSet::new();

    for selector in selectors {
        let selector_kind = classify_selector(selector);
        let matches = match selector_kind {
            SelectorKind::File => {
                let exact = exact_block_matches(selector, document);
                if exact.len() != 1 {
                    return false;
                }
                exact
            }
            SelectorKind::Directory => descendant_block_matches(selector, document),
            SelectorKind::Unknown => {
                let exact = exact_block_matches(selector, document);
                if exact.len() == 1 {
                    exact
                } else {
                    descendant_block_matches(selector, document)
                }
            }
        };

        if matches.is_empty() {
            return false;
        }

        for index in matches {
            covered.insert(index);
        }
    }

    covered.len() == document.blocks.len()
}

fn exact_block_matches(selector: &Path, document: &SoupDocument) -> Vec<usize> {
    document
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| (block.original_absolute_path == selector).then_some(index))
        .collect()
}

fn descendant_block_matches(selector: &Path, document: &SoupDocument) -> Vec<usize> {
    document
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            (block.original_absolute_path.starts_with(selector)
                && block.original_absolute_path != selector)
                .then_some(index)
        })
        .collect()
}

fn partial_block_already_applied(
    existing_lines: &[String],
    range: &SoupPartialRange,
    replacement_lines: &[String],
) -> bool {
    if replacement_lines.is_empty() {
        return false;
    }

    let start = range.start_line;
    let end = start - 1 + replacement_lines.len();
    if end > existing_lines.len() {
        return false;
    }

    existing_lines[start - 1..end] == replacement_lines[..]
}

fn reconstruct_contents(lines: &[String], trailing_newline: bool) -> String {
    let mut contents = lines.join("\n");
    if trailing_newline {
        contents.push('\n');
    }
    contents
}

fn materialize_block_contents(path: &Path, block: &SoupBlock) -> Result<String, SlopError> {
    match &block.partial_range {
        Some(range) => apply_partial_block(path, range, &block.content_lines, block.trailing_newline),
        None => Ok(reconstruct_contents(&block.content_lines, block.trailing_newline)),
    }
}

fn apply_partial_block(
    path: &Path,
    range: &SoupPartialRange,
    replacement_lines: &[String],
    trailing_newline: bool,
) -> Result<String, SlopError> {
    let existing = fs::read_to_string(path).map_err(|error| SlopError::FileReadFailure {
        path: path.to_path_buf(),
        source: error,
    })?;
    let existing_lines = split_existing_lines(&existing);

    if range.end_line > existing_lines.len() {
        return Err(SlopError::SoupParseFailure(format!(
            "partial slop range {}-{} exceeds existing file length {} for {}",
            range.start_line,
            range.end_line,
            existing_lines.len(),
            path.display()
        )));
    }

    if partial_block_already_applied(&existing_lines, range, replacement_lines) {
        eprintln!(
            "note: partial slop block for {} lines {}-{} already applied; skipping (idempotent)",
            path.display(),
            range.start_line,
            range.end_line
        );
        return Ok(existing);
    }

    let mut merged = Vec::with_capacity(
        existing_lines.len() - (range.end_line - range.start_line + 1) + replacement_lines.len(),
    );
    merged.extend(existing_lines[..range.start_line - 1].iter().cloned());
    merged.extend(replacement_lines.iter().cloned());
    merged.extend(existing_lines[range.end_line..].iter().cloned());

    Ok(reconstruct_contents(&merged, trailing_newline))
}

fn split_existing_lines(contents: &str) -> Vec<String> {
    if contents.is_empty() {
        return Vec::new();
    }

    let body = contents.strip_suffix('\n').unwrap_or(contents);
    if body.is_empty() {
        return vec![String::new()];
    }

    body.split('\n').map(ToString::to_string).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectorKind {
    File,
    Directory,
    Unknown,
}

fn classify_selector(selector: &Path) -> SelectorKind {
    match fs::metadata(selector) {
        Ok(metadata) if metadata.is_file() => SelectorKind::File,
        Ok(metadata) if metadata.is_dir() => SelectorKind::Directory,
        Ok(_) => SelectorKind::Unknown,
        Err(_) => SelectorKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::tempdir;

    use crate::config::Config;
    use crate::models::{SoupBlock, SoupDocument, SoupPartialRange};

    use super::{
        apply_partial_block, document_matches, find_matching_slop_file, resolve_direct_slop_document,
    };

    fn document(paths: &[&str]) -> SoupDocument {
        SoupDocument {
            meta_blocks: vec![],
            blocks: paths
                .iter()
                .map(|path| SoupBlock {
                    original_absolute_path: PathBuf::from(path),
                    partial_range: None,
                    logical_line_count: 1,
                    trailing_newline: false,
                    content_lines: vec!["content".to_string()],
                    base_sha: None,
                    read_only: false,
                    block_id: None,
                })
                .collect(),
        }
    }

    #[test]
    fn matches_a_slop_file_for_file_selectors() {
        let doc = document(&["/tmp/one.txt", "/tmp/two.txt"]);
        assert!(document_matches(
            &[PathBuf::from("/tmp/one.txt"), PathBuf::from("/tmp/two.txt")],
            &doc
        ));
    }

    #[test]
    fn matches_a_slop_file_for_directory_selectors() {
        let temp = tempdir().expect("tempdir should exist");
        let directory = temp.path().join("nested");
        fs::create_dir_all(&directory).expect("directory should be created");
        let child = directory.join("file.txt");
        let doc = document(&[child.to_str().expect("utf8 path")]);

        assert!(document_matches(&[directory], &doc));
    }

    #[test]
    fn rejects_zero_matches() {
        let temp = tempdir().expect("tempdir should exist");
        let error = find_matching_slop_file(&[PathBuf::from("/tmp/missing.txt")], temp.path())
            .expect_err("expected no match failure");
        assert!(error.to_string().contains("no matching slop file"));
    }

    #[test]
    fn rejects_multiple_matches() {
        let temp = tempdir().expect("tempdir should exist");
        let selector = PathBuf::from("/tmp/file.txt");
        let header = "#SLOP \"/tmp/file.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 0\nhello";
        fs::write(temp.path().join("one.md"), header).expect("slop file should be written");
        fs::write(temp.path().join("two.md"), header).expect("slop file should be written");

        let error = find_matching_slop_file(&[selector], temp.path())
            .expect_err("expected ambiguous match failure");
        assert!(error.to_string().contains("multiple slop files matched"));
    }

    #[test]
    fn accepts_a_direct_slop_document_path() {
        let temp = tempdir().expect("tempdir should exist");
        let slop_file = temp.path().join("archive.slop");
        fs::write(
            &slop_file,
            "#SLOP \"/tmp/file.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 0\nhello",
        )
        .expect("slop file should be written");

        let direct = resolve_direct_slop_document(std::slice::from_ref(&slop_file))
            .expect("direct slop detection should succeed")
            .expect("direct slop document should be detected");

        assert_eq!(direct.0, slop_file);
        assert_eq!(direct.1.blocks.len(), 1);
    }

    #[test]
    fn ignores_non_slop_file_when_resolving_direct_document() {
        let temp = tempdir().expect("tempdir should exist");
        let source = temp.path().join("file.txt");
        fs::write(&source, "plain text").expect("source file should be written");

        let direct = resolve_direct_slop_document(std::slice::from_ref(&source))
            .expect("non-slop file should not error");

        assert!(direct.is_none());
    }

    #[test]
    fn applies_partial_block_to_existing_file() {
        let temp = tempdir().expect("tempdir should exist");
        let path = temp.path().join("file.txt");
        fs::write(&path, "one\ntwo\nthree\nfour\n").expect("file should be written");

        let updated = apply_partial_block(
            &path,
            &SoupPartialRange {
                start_line: 2,
                end_line: 3,
            },
            &["dos".to_string(), "tres".to_string()],
            true,
        )
        .expect("partial block should apply");

        assert_eq!(updated, "one\ndos\ntres\nfour\n");
    }

    #[test]
    fn rejects_partial_block_that_exceeds_existing_file_length() {
        let temp = tempdir().expect("tempdir should exist");
        let path = temp.path().join("file.txt");
        fs::write(&path, "one\ntwo\n").expect("file should be written");

        let error = apply_partial_block(
            &path,
            &SoupPartialRange {
                start_line: 2,
                end_line: 4,
            },
            &["dos".to_string()],
            true,
        )
        .expect_err("partial block should fail");

        assert!(error
            .to_string()
            .contains("partial slop range 2-4 exceeds existing file length 2"));
    }

    #[test]
    fn deslop_skips_meta_blocks() {
        let temp = tempdir().expect("tempdir should exist");
        let slop_file = temp.path().join("with_meta.md");
        fs::write(
            &slop_file,
            "#SLOP_META \"repo-graph\" #SLOP_META_KIND codegraph #SLOP_META_FORMAT repomap #SLOP_META_LINES 2 #SLOP_META_READONLY true\ngraph_line1\ngraph_line2\n#SLOP \"/tmp/file.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 0\nhello",
        )
        .expect("slop file should be written");

        let direct = resolve_direct_slop_document(std::slice::from_ref(&slop_file))
            .expect("direct slop detection should succeed")
            .expect("direct slop document should be detected");

        assert_eq!(direct.1.meta_blocks.len(), 1);
        assert_eq!(direct.1.blocks.len(), 1);
        assert_eq!(direct.1.blocks[0].content_lines, vec!["hello"]);
    }

    #[test]
    fn partial_block_is_idempotent_when_replacement_adds_a_line() {
        let temp = tempdir().expect("tempdir should exist");
        let path = temp.path().join("file.txt");
        fs::write(&path, "line1\nline2\nline3\nline4\n").expect("seed file should be written");

        let range = SoupPartialRange {
            start_line: 2,
            end_line: 3,
        };
        let replacement = vec![
            "line2 changed".to_string(),
            "line3 changed".to_string(),
            "line 3.1 new line!".to_string(),
        ];

        let first = apply_partial_block(&path, &range, &replacement, true)
            .expect("first apply should succeed");
        assert_eq!(
            first,
            "line1\nline2 changed\nline3 changed\nline 3.1 new line!\nline4\n"
        );
        fs::write(&path, &first).expect("first application should be persisted");

        let second = apply_partial_block(&path, &range, &replacement, true)
            .expect("second apply should succeed");

        assert_eq!(
            second, first,
            "re-running deslop on an already-applied partial block must leave the file unchanged"
        );
    }

    #[test]
    fn partial_block_is_idempotent_when_replacement_removes_a_line() {
        let temp = tempdir().expect("tempdir should exist");
        let path = temp.path().join("file.txt");
        fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").expect("seed file should be written");

        let range = SoupPartialRange {
            start_line: 2,
            end_line: 3,
        };
        let replacement = vec!["beta only".to_string()];

        let first = apply_partial_block(&path, &range, &replacement, true)
            .expect("first apply should succeed");
        assert_eq!(first, "alpha\nbeta only\ndelta\n");
        fs::write(&path, &first).expect("first application should be persisted");

        let second = apply_partial_block(&path, &range, &replacement, true)
            .expect("second apply should succeed");

        assert_eq!(
            second, first,
            "re-running deslop after a line-removing partial block must not corrupt the file"
        );
    }

    #[test]
    fn partial_block_reapplies_when_content_does_not_match() {
        let temp = tempdir().expect("tempdir should exist");
        let path = temp.path().join("file.txt");
        fs::write(&path, "one\ntwo\nthree\n").expect("seed file should be written");

        let range = SoupPartialRange {
            start_line: 2,
            end_line: 2,
        };
        let replacement = vec!["two updated".to_string()];

        let result = apply_partial_block(&path, &range, &replacement, true)
            .expect("apply should succeed when content has not been applied yet");

        assert_eq!(result, "one\ntwo updated\nthree\n");
    }

    #[test]
    fn block_id_is_stable_for_identical_content() {
        use super::block_id_for;
        use crate::models::SoupBlock;

        let mk = |content: &[&str], trailing: bool| SoupBlock {
            original_absolute_path: PathBuf::from("/anywhere/file.txt"),
            partial_range: None,
            logical_line_count: content.len(),
            trailing_newline: trailing,
            content_lines: content.iter().map(|s| s.to_string()).collect(),
            base_sha: None,
            read_only: false,
            block_id: None,
        };

        let a = mk(&["alpha", "beta"], true);
        let b = mk(&["alpha", "beta"], true);
        let c = mk(&["alpha", "beta"], false);
        let d = mk(&["alpha", "gamma"], true);

        assert_eq!(block_id_for(&a), block_id_for(&b), "identical content + trailing must hash equal");
        assert_ne!(block_id_for(&a), block_id_for(&c), "trailing newline must affect the id");
        assert_ne!(block_id_for(&a), block_id_for(&d), "different content must hash differently");
    }

    #[test]
    fn block_id_is_independent_of_path() {
        use super::block_id_for;
        use crate::models::SoupBlock;

        let mk = |path: &str| SoupBlock {
            original_absolute_path: PathBuf::from(path),
            partial_range: None,
            logical_line_count: 1,
            trailing_newline: false,
            content_lines: vec!["same".to_string()],
            base_sha: None,
            read_only: false,
            block_id: None,
        };

        assert_eq!(
            block_id_for(&mk("/a/b.txt")),
            block_id_for(&mk("/x/y.txt")),
            "id is hashed from content, not path"
        );
    }

    #[test]
    fn cache_records_and_detects_block_ids() {
        use super::DeslopCache;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("cache");
        let mut cache = DeslopCache::load(path.clone());

        assert!(!cache.contains("aaaa1111"));
        cache.record("aaaa1111".to_string());
        assert!(cache.contains("aaaa1111"));
        cache.persist();
        assert!(path.is_file(), "cache file should be persisted");
    }

    #[test]
    fn cache_evicts_oldest_beyond_limit() {
        use super::{DeslopCache, DESLOP_CACHE_LIMIT};

        let temp = tempdir().expect("tempdir");
        let mut cache = DeslopCache::load(temp.path().join("cache"));

        for i in 0..DESLOP_CACHE_LIMIT {
            cache.record(format!("block-{i:04}"));
        }
        assert!(cache.contains("block-0000"));
        assert!(cache.contains(&format!("block-{:04}", DESLOP_CACHE_LIMIT - 1)));

        cache.record("new-block".to_string());
        assert!(!cache.contains("block-0000"), "oldest entry must be evicted");
        assert!(cache.contains("new-block"));
        assert_eq!(
            cache.entry_count(),
            DESLOP_CACHE_LIMIT,
            "cache must stay at the configured limit"
        );
    }

    #[test]
    fn cache_persists_across_loads() {
        use super::DeslopCache;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("cache");

        {
            let mut cache = DeslopCache::load(path.clone());
            cache.record("persisted-id".to_string());
            cache.persist();
        }

        let reloaded = DeslopCache::load(path);
        assert!(reloaded.contains("persisted-id"));
    }

    #[test]
    fn cache_dedupes_repeated_record_calls() {
        use super::DeslopCache;

        let temp = tempdir().expect("tempdir");
        let mut cache = DeslopCache::load(temp.path().join("cache"));

        cache.record("dup".to_string());
        cache.record("dup".to_string());

        assert_eq!(cache.entry_count(), 1);
    }

    #[test]
    fn cache_key_is_scoped_by_destination_path() {
        use super::cache_key;

        let key_a = cache_key(Path::new("/a/__init__.py"), "sameblockid");
        let key_b = cache_key(Path::new("/b/__init__.py"), "sameblockid");
        assert_ne!(
            key_a, key_b,
            "identical content at two paths must not collide in the ledger"
        );
        assert_eq!(key_a, cache_key(Path::new("/a/__init__.py"), "sameblockid"));
    }

    #[test]
    fn file_already_matches_detects_identical_and_missing_files() {
        use super::file_already_matches;

        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("target.txt");

        assert!(
            !file_already_matches(&path, "body\n"),
            "a missing file never matches"
        );

        fs::write(&path, "body\n").expect("file should be written");
        assert!(file_already_matches(&path, "body\n"));
        assert!(!file_already_matches(&path, "different\n"));
        assert!(
            !file_already_matches(&path, "body"),
            "the trailing newline is part of the comparison"
        );
    }

    #[test]
    fn cache_disabled_by_default_and_resolves_under_dot_slop() {
        use super::resolve_cache_path;

        // SAFETY: these tests run single-threaded; mutating env is safe.
        unsafe {
            std::env::remove_var("SLOP_DESLOP_CACHE");
            std::env::set_var("HOME", "/home/example");
        }

        let mut config = Config::default();
        assert!(
            resolve_cache_path(&config).is_none(),
            "the ledger is opt-in; content comparison covers idempotency"
        );

        config.deslop_cache = true;
        assert_eq!(
            resolve_cache_path(&config),
            Some(PathBuf::from("/home/example/.slop/.slop_blocks_cache"))
        );

        config.deslop_cache_path = Some(PathBuf::from("~/custom/blocks"));
        assert_eq!(
            resolve_cache_path(&config),
            Some(PathBuf::from("/home/example/custom/blocks"))
        );

        unsafe {
            std::env::set_var("HOME", "/tmp");
        }
    }

    #[test]
    fn cache_env_var_overrides_config_and_preserves_case() {
        use super::resolve_cache_path;

        let config = Config::default();

        // SAFETY: these tests run single-threaded; mutating env is safe.
        unsafe {
            std::env::set_var("SLOP_DESLOP_CACHE", "/Tmp/MixedCase/Blocks");
        }
        assert_eq!(
            resolve_cache_path(&config),
            Some(PathBuf::from("/Tmp/MixedCase/Blocks")),
            "cache paths must not be lowercased"
        );

        for disabled in ["off", "OFF", "disabled", "none", "false", "  "] {
            unsafe {
                std::env::set_var("SLOP_DESLOP_CACHE", disabled);
            }
            assert!(resolve_cache_path(&config).is_none());
        }

        unsafe {
            std::env::remove_var("SLOP_DESLOP_CACHE");
        }
    }
}
