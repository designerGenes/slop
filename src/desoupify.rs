use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::SoupifyError;
use crate::models::{CliArgs, SoupBlock, SoupDocument, SoupMatchResult, SoupPartialRange};
use crate::pathing::{resolve_absolute, resolve_output_dir};
use crate::soup_format::parse_document;

pub fn find_matching_soup_file(
    selectors: &[PathBuf],
    soup_dir: &Path,
) -> Result<PathBuf, SoupifyError> {
    match match_soup_file(selectors, soup_dir)? {
        SoupMatchResult::One(path) => Ok(path),
        SoupMatchResult::None => Err(SoupifyError::NoMatchingSoupFile {
            selectors: selectors.to_vec(),
            soup_dir: soup_dir.to_path_buf(),
        }),
        SoupMatchResult::Ambiguous(paths) => Err(SoupifyError::AmbiguousSoupFileMatch { paths }),
    }
}

pub fn run_desoupify(args: &CliArgs, config: &Config) -> Result<Vec<PathBuf>, SoupifyError> {
    let cwd = std::env::current_dir().map_err(|error| SoupifyError::FileReadFailure {
        path: PathBuf::from("."),
        source: error,
    })?;
    let soup_dir = resolve_output_dir(
        args.output_dir
            .as_deref()
            .or(args.soupify_to.as_deref())
            .or(config.soupified_folder.as_deref()),
        &cwd,
    )?;
    let resolved_inputs = args
        .inputs
        .iter()
        .map(|selector| resolve_absolute(selector, &cwd))
        .collect::<Result<Vec<_>, _>>()?;

    let (_soup_file, document) = match resolve_direct_soup_document(&resolved_inputs)? {
        Some((soup_file, document)) => (soup_file, document),
        None => {
            let soup_file = find_matching_soup_file(&resolved_inputs, &soup_dir)?;
            let document = read_soup_document(&soup_file)?;
            (soup_file, document)
        }
    };

    if !document.meta_blocks.is_empty() {
        eprintln!(
            "warning: {} #SOUP_META block(s) found in soup; these are reference-only and will be skipped during desoupify",
            document.meta_blocks.len()
        );
    }

    let allowed_roots = compute_allowed_roots(&document.blocks, &args.allow_roots, &cwd);

    let mut restored_paths = Vec::with_capacity(document.blocks.len());
    for block in document.blocks {
        let restored_path = block.original_absolute_path.clone();

        if block.read_only {
            eprintln!("warning: read-only block for {} skipped in desoupify", restored_path.display());
            continue;
        }

        if let Some(ref sha) = block.base_sha {
            if block.partial_range.is_some() {
                if let Ok(on_disk_bytes) = fs::read(&restored_path) {
                    let actual = blake3::hash(&on_disk_bytes).to_hex().to_string();
                    if actual != *sha {
                        eprintln!(
                            "warning: base SHA drift for {}: expected {}, got {}; applying partial block anyway",
                            restored_path.display(), sha, actual
                        );
                    }
                }
            }
        }

        if !is_within_allowed_roots(&restored_path, &allowed_roots) {
            return Err(SoupifyError::WriteOutsideAllowedRoot {
                path: restored_path.clone(),
                allowed_roots: allowed_roots.clone(),
            });
        }

        if block.base_sha.is_none() && !block.read_only {
            eprintln!(
                "warning: novel file in returned soup: {} (no base SHA)",
                restored_path.display()
            );
        }

        if args.dry_run {
            let contents = materialize_block_contents(&restored_path, &block)?;
            let existing = fs::read_to_string(&restored_path).unwrap_or_default();
            let diff = unified_diff(&existing, &contents, &restored_path);
            if !diff.is_empty() {
                println!("{}", diff);
            }
            restored_paths.push(restored_path);
            continue;
        }

        if let Some(parent) = restored_path.parent() {
            fs::create_dir_all(parent).map_err(|error| SoupifyError::DirectoryCreationFailure {
                path: parent.to_path_buf(),
                source: error,
            })?;
        }

        let contents = materialize_block_contents(&restored_path, &block)?;
        fs::write(&restored_path, contents).map_err(|error| SoupifyError::FileWriteFailure {
            path: restored_path.clone(),
            source: error,
        })?;
        restored_paths.push(restored_path);
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
    format!("--- {} (current)\n+++ {} (soup)\n{}", path.display(), path.display(), result)
}

fn resolve_direct_soup_document(
    inputs: &[PathBuf],
) -> Result<Option<(PathBuf, SoupDocument)>, SoupifyError> {
    let [input] = inputs else {
        return Ok(None);
    };

    if !input.is_file() || !looks_like_soup_file(input) {
        return Ok(None);
    }

    read_soup_document(input).map(|document| Some((input.clone(), document)))
}

fn read_soup_document(path: &Path) -> Result<SoupDocument, SoupifyError> {
    let markdown = fs::read_to_string(path).map_err(|error| SoupifyError::FileReadFailure {
        path: path.to_path_buf(),
        source: error,
    })?;
    parse_document(&markdown)
}

fn looks_like_soup_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    contents.lines().next().map_or(false, |first_line| {
        first_line.starts_with("#SOUP ")
            || first_line.starts_with("#SOUP_META ")
            || first_line.starts_with("#SOUP_AUTO_UNSOUPIFY")
    })
}

fn match_soup_file(
    selectors: &[PathBuf],
    soup_dir: &Path,
) -> Result<SoupMatchResult, SoupifyError> {
    let candidates = collect_candidate_soup_files(soup_dir)?;
    let mut matches = Vec::new();

    for candidate in candidates {
        let markdown =
            fs::read_to_string(&candidate).map_err(|error| SoupifyError::FileReadFailure {
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

fn collect_candidate_soup_files(soup_dir: &Path) -> Result<Vec<PathBuf>, SoupifyError> {
    let directory_entries = match fs::read_dir(soup_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(SoupifyError::FileReadFailure {
                path: soup_dir.to_path_buf(),
                source: error,
            });
        }
    };

    let mut files = Vec::new();
    for entry in directory_entries {
        let entry = entry.map_err(|error| SoupifyError::FileReadFailure {
            path: soup_dir.to_path_buf(),
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

fn materialize_block_contents(path: &Path, block: &SoupBlock) -> Result<String, SoupifyError> {
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
) -> Result<String, SoupifyError> {
    let existing = fs::read_to_string(path).map_err(|error| SoupifyError::FileReadFailure {
        path: path.to_path_buf(),
        source: error,
    })?;
    let existing_lines = split_existing_lines(&existing);

    if range.end_line > existing_lines.len() {
        return Err(SoupifyError::SoupParseFailure(format!(
            "partial soup range {}-{} exceeds existing file length {} for {}",
            range.start_line,
            range.end_line,
            existing_lines.len(),
            path.display()
        )));
    }

    if partial_block_already_applied(&existing_lines, range, replacement_lines) {
        eprintln!(
            "note: partial soup block for {} lines {}-{} already applied; skipping (idempotent)",
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
    use std::path::PathBuf;

    use tempfile::tempdir;

    use crate::models::{SoupBlock, SoupDocument, SoupPartialRange};

    use super::{
        apply_partial_block, document_matches, find_matching_soup_file, resolve_direct_soup_document,
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
                })
                .collect(),
        }
    }

    #[test]
    fn matches_a_soup_file_for_file_selectors() {
        let doc = document(&["/tmp/one.txt", "/tmp/two.txt"]);
        assert!(document_matches(
            &[PathBuf::from("/tmp/one.txt"), PathBuf::from("/tmp/two.txt")],
            &doc
        ));
    }

    #[test]
    fn matches_a_soup_file_for_directory_selectors() {
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
        let error = find_matching_soup_file(&[PathBuf::from("/tmp/missing.txt")], temp.path())
            .expect_err("expected no match failure");
        assert!(error.to_string().contains("no matching soup file"));
    }

    #[test]
    fn rejects_multiple_matches() {
        let temp = tempdir().expect("tempdir should exist");
        let selector = PathBuf::from("/tmp/file.txt");
        let header = "#SOUP \"/tmp/file.txt\" #SOUPED_LINES 1 #SOUP_TRAILING_NEWLINE 0\nhello";
        fs::write(temp.path().join("one.md"), header).expect("soup file should be written");
        fs::write(temp.path().join("two.md"), header).expect("soup file should be written");

        let error = find_matching_soup_file(&[selector], temp.path())
            .expect_err("expected ambiguous match failure");
        assert!(error.to_string().contains("multiple soup files matched"));
    }

    #[test]
    fn accepts_a_direct_soup_document_path() {
        let temp = tempdir().expect("tempdir should exist");
        let soup_file = temp.path().join("archive.soup");
        fs::write(
            &soup_file,
            "#SOUP \"/tmp/file.txt\" #SOUPED_LINES 1 #SOUP_TRAILING_NEWLINE 0\nhello",
        )
        .expect("soup file should be written");

        let direct = resolve_direct_soup_document(std::slice::from_ref(&soup_file))
            .expect("direct soup detection should succeed")
            .expect("direct soup document should be detected");

        assert_eq!(direct.0, soup_file);
        assert_eq!(direct.1.blocks.len(), 1);
    }

    #[test]
    fn ignores_non_soup_file_when_resolving_direct_document() {
        let temp = tempdir().expect("tempdir should exist");
        let source = temp.path().join("file.txt");
        fs::write(&source, "plain text").expect("source file should be written");

        let direct = resolve_direct_soup_document(std::slice::from_ref(&source))
            .expect("non-soup file should not error");

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
            .contains("partial soup range 2-4 exceeds existing file length 2"));
    }

    #[test]
    fn desoupify_skips_meta_blocks() {
        let temp = tempdir().expect("tempdir should exist");
        let soup_file = temp.path().join("with_meta.md");
        fs::write(
            &soup_file,
            "#SOUP_META \"repo-graph\" #SOUP_META_KIND codegraph #SOUP_META_FORMAT repomap #SOUP_META_LINES 2 #SOUP_META_READONLY true\ngraph_line1\ngraph_line2\n#SOUP \"/tmp/file.txt\" #SOUPED_LINES 1 #SOUP_TRAILING_NEWLINE 0\nhello",
        )
        .expect("soup file should be written");

        let direct = resolve_direct_soup_document(std::slice::from_ref(&soup_file))
            .expect("direct soup detection should succeed")
            .expect("direct soup document should be detected");

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
            "re-running desoupify on an already-applied partial block must leave the file unchanged"
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
            "re-running desoupify after a line-removing partial block must not corrupt the file"
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
}
