use std::path::PathBuf;
use std::sync::OnceLock;

use regex::Regex;

use crate::error::SlopError;
use crate::models::{SoupBlock, SoupDocument, SoupMetaBlock, SoupPartialRange, SourceFile};

pub fn serialize_document(
    meta_blocks: &[SoupMetaBlock],
    files: &[SourceFile],
) -> Result<String, SlopError> {
    let mut lines = Vec::new();

    for meta in meta_blocks {
        lines.push(serialize_meta_header(meta));
        for line in &meta.content_lines {
            lines.push(line.clone());
        }
    }

    for file in files {
        lines.push(serialize_header(file)?);
        for line in content_lines(&file.contents) {
            lines.push(line);
        }
    }

    Ok(lines.join("\n"))
}

pub fn parse_document(markdown: &str) -> Result<SoupDocument, SlopError> {
    if markdown.is_empty() {
        return Ok(SoupDocument {
            meta_blocks: Vec::new(),
            blocks: Vec::new(),
        });
    }

    let lines: Vec<&str> = markdown.split('\n').collect();

    let mut meta_blocks = Vec::new();
    let mut blocks = Vec::new();
    let mut index = 0usize;

    while index < lines.len() {
        let header = lines[index];
        if header.is_empty() && index == lines.len() - 1 {
            break;
        }

        if header.starts_with("#SLOP_AUTO_UNslop") {
            index += 1;
            continue;
        }

        if header.starts_with("#SLOP_META ") {
            let (meta_block, consumed) = parse_meta_block(header, index + 1, &lines)?;
            index += 1 + consumed;
            meta_blocks.push(meta_block);
            continue;
        }

        let (path, partial_range, logical_line_count, trailing_newline, base_sha, read_only) =
            parse_header(header, index + 1)?;
        index += 1;

        if lines.len().saturating_sub(index) < logical_line_count {
            return Err(SlopError::SoupParseFailure(format!(
                "declared line count {} exceeds available lines for {}",
                logical_line_count,
                path.display()
            )));
        }

        let mut content_lines = Vec::with_capacity(logical_line_count);
        for _ in 0..logical_line_count {
            content_lines.push(lines[index].to_string());
            index += 1;
        }

        blocks.push(SoupBlock {
            original_absolute_path: path,
            partial_range,
            logical_line_count,
            trailing_newline,
            content_lines,
            base_sha,
            read_only,
        });
    }

    Ok(SoupDocument { meta_blocks, blocks })
}

fn serialize_meta_header(meta: &SoupMetaBlock) -> String {
    format!(
        "#SLOP_META \"{}\" #SLOP_META_KIND {} #SLOP_META_FORMAT {} #SLOP_META_LINES {} #SLOP_META_READONLY {}",
        meta.label, meta.kind, meta.format, meta.line_count, meta.readonly
    )
}

fn parse_meta_block(
    header: &str,
    line_number: usize,
    lines: &[&str],
) -> Result<(SoupMetaBlock, usize), SlopError> {
    let captures = meta_header_regex().captures(header).ok_or_else(|| {
        SlopError::SoupParseFailure(format!(
            "malformed slop meta header on line {line_number}: {header}"
        ))
    })?;

    let label = captures.get(1).unwrap().as_str().to_string();
    let kind = captures.get(2).unwrap().as_str().to_string();
    let format = captures.get(3).unwrap().as_str().to_string();
    let line_count = captures
        .get(4)
        .unwrap()
        .as_str()
        .parse::<usize>()
        .map_err(|error| {
            SlopError::SoupParseFailure(format!(
                "invalid meta line count on line {line_number}: {error}"
            ))
        })?;
    let readonly = match captures.get(5).unwrap().as_str() {
        "true" => true,
        "false" => false,
        other => {
            return Err(SlopError::SoupParseFailure(format!(
                "invalid readonly marker on line {line_number}: {other}"
            )));
        }
    };

    let start = line_number;
    if lines.len().saturating_sub(start) < line_count {
        return Err(SlopError::SoupParseFailure(format!(
            "declared meta line count {line_count} exceeds available lines on line {line_number}"
        )));
    }

    let mut content_lines = Vec::with_capacity(line_count);
    for i in 0..line_count {
        content_lines.push(lines[start + i].to_string());
    }

    Ok((
        SoupMetaBlock {
            label,
            kind,
            format,
            line_count,
            readonly,
            content_lines,
        },
        line_count,
    ))
}

fn serialize_header(file: &SourceFile) -> Result<String, SlopError> {
    let path = serde_json::to_string(&file.original_absolute_path.to_string_lossy().to_string())
        .map_err(|error| SlopError::SoupParseFailure(error.to_string()))?;

    let mut header = format!(
        "#SLOP {path} #SLOPED_LINES {} #SLOP_TRAILING_NEWLINE {}",
        file.logical_line_count,
        usize::from(file.trailing_newline)
    );

    if let Some(ref sha) = file.base_sha {
        header.push_str(&format!(" #SLOP_BASE_SHA {sha}"));
    }

    if file.read_only {
        header.push_str(" #SLOP_READONLY true");
    }

    Ok(header)
}

pub(crate) fn content_lines(contents: &str) -> Vec<String> {
    if contents.is_empty() {
        return Vec::new();
    }

    let trailing_newline = contents.ends_with('\n');
    let body = if trailing_newline {
        &contents[..contents.len() - 1]
    } else {
        contents
    };

    if body.is_empty() {
        return vec![String::new()];
    }

    body.split('\n').map(ToString::to_string).collect()
}

pub fn analyze_contents(contents: &str) -> (usize, bool) {
    let lines = content_lines(contents);
    (
        lines.len(),
        contents.ends_with('\n') && !contents.is_empty(),
    )
}

fn parse_header(
    line: &str,
    line_number: usize,
) -> Result<(PathBuf, Option<SoupPartialRange>, usize, bool, Option<String>, bool), SlopError> {
    let path_captures = header_path_regex().captures(line).ok_or_else(|| {
        SlopError::SoupParseFailure(format!(
            "malformed slop header on line {line_number}: {line}"
        ))
    })?;

    let has_line_count_metadata = line.contains(" #SLOPED_LINES ");
    let has_trailing_newline_metadata = line.contains(" #SLOP_TRAILING_NEWLINE ");
    if (has_line_count_metadata || has_trailing_newline_metadata)
        && !(has_line_count_metadata && has_trailing_newline_metadata)
    {
        return Err(SlopError::SoupParseFailure(format!(
            "missing slop metadata on line {line_number}: {line}"
        )));
    }

    let captures = header_regex().captures(line).ok_or_else(|| {
        SlopError::SoupParseFailure(format!(
            "malformed slop header on line {line_number}: {line}"
        ))
    })?;

    let path_json = path_captures
        .get(1)
        .map(|capture| capture.as_str())
        .ok_or_else(|| {
            SlopError::SoupParseFailure(format!(
                "slop header missing path metadata on line {line_number}"
            ))
        })?;
    let path: String = serde_json::from_str(path_json).map_err(|error| {
        SlopError::SoupParseFailure(format!(
            "invalid escaped slop path on line {line_number}: {error}"
        ))
    })?;

    let partial_range = match (captures.get(2), captures.get(3)) {
        (Some(start), Some(end)) => {
            let start_line = start.as_str().parse::<usize>().map_err(|error| {
                SlopError::SoupParseFailure(format!(
                    "invalid partial slop start line on line {line_number}: {error}"
                ))
            })?;
            let end_line = end.as_str().parse::<usize>().map_err(|error| {
                SlopError::SoupParseFailure(format!(
                    "invalid partial slop end line on line {line_number}: {error}"
                ))
            })?;

            if start_line == 0 || end_line == 0 || start_line > end_line {
                return Err(SlopError::SoupParseFailure(format!(
                    "invalid partial slop range on line {line_number}: {line}"
                )));
            }

            Some(SoupPartialRange {
                start_line,
                end_line,
            })
        }
        (None, None) => None,
        _ => {
            return Err(SlopError::SoupParseFailure(format!(
                "malformed slop header on line {line_number}: {line}"
            )));
        }
    };

    let logical_line_count = captures
        .get(4)
        .ok_or_else(|| {
            SlopError::SoupParseFailure(format!(
                "slop header missing line count on line {line_number}"
            ))
        })?
        .as_str()
        .parse::<usize>()
        .map_err(|error| {
            SlopError::SoupParseFailure(format!(
                "invalid slop line count on line {line_number}: {error}"
            ))
        })?;

    let trailing_newline = match captures.get(5).map(|capture| capture.as_str()) {
        Some("0") | Some("false") => false,
        Some("1") | Some("true") => true,
        Some(other) => {
            return Err(SlopError::SoupParseFailure(format!(
                "invalid trailing newline marker on line {line_number}: {other}"
            )));
        }
        None => {
            return Err(SlopError::SoupParseFailure(format!(
                "slop header missing trailing newline metadata on line {line_number}"
            )));
        }
    };

    let base_sha = captures.get(6).map(|m| m.as_str().to_string());

    let read_only = captures
        .get(7)
        .map(|m| m.as_str() == "true")
        .unwrap_or(false);

    Ok((
        PathBuf::from(path),
        partial_range,
        logical_line_count,
        trailing_newline,
        base_sha,
        read_only,
    ))
}

fn header_path_regex() -> &'static Regex {
    static HEADER_PATH_REGEX: OnceLock<Regex> = OnceLock::new();
    HEADER_PATH_REGEX.get_or_init(|| {
        Regex::new(r#"^#SLOP ("(?:\\.|[^"\\])*")(?: .*)?$"#)
            .expect("header path regex should compile")
    })
}

fn header_regex() -> &'static Regex {
    static HEADER_REGEX: OnceLock<Regex> = OnceLock::new();
    HEADER_REGEX.get_or_init(|| {
        Regex::new(
            r#"^#SLOP ("(?:\\.|[^"\\])*")(?: #SLOP_PARTIAL_LINES ([0-9]+)-([0-9]+))? #SLOPED_LINES ([0-9]+) #SLOP_TRAILING_NEWLINE (0|1|true|false)(?: #SLOP_BASE_SHA ([0-9a-f]{64}))?(?: #SLOP_READONLY (true|false))?$"#,
        )
        .expect("header regex should compile")
    })
}

fn meta_header_regex() -> &'static Regex {
    static META_HEADER_REGEX: OnceLock<Regex> = OnceLock::new();
    META_HEADER_REGEX.get_or_init(|| {
        Regex::new(
            r#"^#SLOP_META "(.+?)" #SLOP_META_KIND (\S+) #SLOP_META_FORMAT (\S+) #SLOP_META_LINES (\d+) #SLOP_META_READONLY (true|false)$"#,
        )
        .expect("meta header regex should compile")
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::models::{SoupMetaBlock, SoupPartialRange, SourceFile};

    use super::{analyze_contents, parse_document, serialize_document};

    fn source_file(path: &str, contents: &str) -> SourceFile {
        let (logical_line_count, trailing_newline) = analyze_contents(contents);
        SourceFile {
            original_absolute_path: PathBuf::from(path),
            file_name: PathBuf::from(path)
                .file_name()
                .expect("basename should exist")
                .to_string_lossy()
                .to_string(),
            name_token: "token".to_string(),
            contents: contents.to_string(),
            logical_line_count,
            trailing_newline,
            base_sha: None,
            read_only: false,
        }
    }

    #[test]
    fn serializes_a_single_block_correctly() {
        let document = serialize_document(&[], &[source_file("/tmp/file.txt", "hello")])
            .expect("document should serialize");
        assert_eq!(
            document,
            "#SLOP \"/tmp/file.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 0\nhello"
        );
    }

    #[test]
    fn serializes_multiple_blocks_correctly() {
        let document = serialize_document(
            &[],
            &[
                source_file("/tmp/one.txt", "one\n"),
                source_file("/tmp/two.txt", "two"),
            ],
        )
        .expect("document should serialize");

        assert_eq!(
            document,
            "#SLOP \"/tmp/one.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1\none\n#SLOP \"/tmp/two.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 0\ntwo"
        );
    }

    #[test]
    fn parses_valid_headers() {
        let document = parse_document(
            "#SLOP \"/tmp/file.txt\" #SLOPED_LINES 2 #SLOP_TRAILING_NEWLINE 0\na\nb",
        )
        .expect("document should parse");

        assert_eq!(document.blocks.len(), 1);
        assert_eq!(
            document.blocks[0].original_absolute_path,
            PathBuf::from("/tmp/file.txt")
        );
        assert_eq!(document.blocks[0].logical_line_count, 2);
        assert!(!document.blocks[0].trailing_newline);
        assert_eq!(document.blocks[0].partial_range, None);
    }

    #[test]
    fn parses_partial_headers() {
        let document = parse_document(
            "#SLOP \"/tmp/file.txt\" #SLOP_PARTIAL_LINES 2-3 #SLOPED_LINES 2 #SLOP_TRAILING_NEWLINE 1\na\nb",
        )
        .expect("document should parse");

        assert_eq!(document.blocks.len(), 1);
        assert_eq!(
            document.blocks[0].partial_range,
            Some(SoupPartialRange {
                start_line: 2,
                end_line: 3,
            })
        );
    }

    #[test]
    fn parses_boolean_trailing_newline_markers() {
        let with_newline = parse_document(
            "#SLOP \"/tmp/true.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE true\na",
        )
        .expect("document should parse");
        let without_newline = parse_document(
            "#SLOP \"/tmp/false.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE false\na",
        )
        .expect("document should parse");

        assert!(with_newline.blocks[0].trailing_newline);
        assert!(!without_newline.blocks[0].trailing_newline);
    }

    #[test]
    fn rejects_malformed_headers() {
        let error =
            parse_document("#SLOP \"/tmp/file.txt\"\na").expect_err("malformed header should fail");
        assert!(error.to_string().contains("malformed slop header"));
    }

    #[test]
    fn rejects_headers_missing_required_metadata() {
        let error = parse_document("#SLOP \"/tmp/file.txt\" #SLOPED_LINES 1\na")
            .expect_err("missing metadata should fail");
        assert!(error.to_string().contains("missing slop metadata"));
    }

    #[test]
    fn rejects_invalid_partial_ranges() {
        let error = parse_document(
            "#SLOP \"/tmp/file.txt\" #SLOP_PARTIAL_LINES 3-2 #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 0\na",
        )
        .expect_err("invalid partial range should fail");
        assert!(error.to_string().contains("invalid partial slop range"));
    }

    #[test]
    fn preserves_empty_files() {
        let document =
            parse_document("#SLOP \"/tmp/empty.txt\" #SLOPED_LINES 0 #SLOP_TRAILING_NEWLINE 0")
                .expect("document should parse");

        assert!(document.blocks[0].content_lines.is_empty());
    }

    #[test]
    fn preserves_files_with_and_without_trailing_newline() {
        let with_newline = source_file("/tmp/with.txt", "hello\n");
        let without_newline = source_file("/tmp/without.txt", "hello");
        let document = serialize_document(&[], &[with_newline, without_newline])
            .expect("document should serialize");
        let parsed = parse_document(&document).expect("document should parse");

        assert!(parsed.blocks[0].trailing_newline);
        assert!(!parsed.blocks[1].trailing_newline);
    }

    #[test]
    fn round_trips_document_exactly() {
        let files = vec![
            source_file("/tmp/empty.txt", ""),
            source_file("/tmp/multi.txt", "one\n\nthree\n"),
        ];

        let markdown = serialize_document(&[], &files).expect("document should serialize");
        let parsed = parse_document(&markdown).expect("document should parse");

        assert_eq!(parsed.blocks.len(), 2);
        assert_eq!(parsed.blocks[0].logical_line_count, 0);
        assert_eq!(parsed.blocks[1].content_lines, vec!["one", "", "three"]);
        assert!(parsed.blocks[1].trailing_newline);
    }

    #[test]
    fn accepts_a_trailing_newline_after_the_last_block() {
        let document = parse_document("#SLOP \"/tmp/file.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE true\nhello\n")
            .expect("document should parse");

        assert_eq!(document.blocks.len(), 1);
        assert_eq!(document.blocks[0].content_lines, vec!["hello"]);
        assert!(document.blocks[0].trailing_newline);
    }

    #[test]
    fn serializes_meta_block_before_file_blocks() {
        let meta = SoupMetaBlock {
            label: "repo-graph".to_string(),
            kind: "codegraph".to_string(),
            format: "repomap".to_string(),
            line_count: 2,
            readonly: true,
            content_lines: vec!["line1".to_string(), "line2".to_string()],
        };
        let files = vec![source_file("/tmp/file.txt", "hello")];
        let document =
            serialize_document(&[meta], &files).expect("document should serialize");

        assert!(document.starts_with("#SLOP_META \"repo-graph\""));
        assert!(document.contains("line1\nline2\n#SLOP"));
    }

    #[test]
    fn parses_meta_block_correctly() {
        let input = "#SLOP_META \"repo-graph\" #SLOP_META_KIND codegraph #SLOP_META_FORMAT repomap #SLOP_META_LINES 3 #SLOP_META_READONLY true\nline1\nline2\nline3\n#SLOP \"/tmp/file.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 0\nhello";
        let document = parse_document(input).expect("document should parse");

        assert_eq!(document.meta_blocks.len(), 1);
        assert_eq!(document.meta_blocks[0].label, "repo-graph");
        assert_eq!(document.meta_blocks[0].kind, "codegraph");
        assert_eq!(document.meta_blocks[0].format, "repomap");
        assert_eq!(document.meta_blocks[0].line_count, 3);
        assert!(document.meta_blocks[0].readonly);
        assert_eq!(
            document.meta_blocks[0].content_lines,
            vec!["line1", "line2", "line3"]
        );

        assert_eq!(document.blocks.len(), 1);
        assert_eq!(document.blocks[0].content_lines, vec!["hello"]);
    }

    #[test]
    fn round_trips_meta_blocks() {
        let meta = SoupMetaBlock {
            label: "test-graph".to_string(),
            kind: "codegraph".to_string(),
            format: "dot".to_string(),
            line_count: 2,
            readonly: true,
            content_lines: vec!["digraph {".to_string(), "}".to_string()],
        };
        let files = vec![source_file("/tmp/file.txt", "content\n")];

        let markdown =
            serialize_document(&[meta], &files).expect("document should serialize");
        let parsed = parse_document(&markdown).expect("document should parse");

        assert_eq!(parsed.meta_blocks.len(), 1);
        assert_eq!(parsed.meta_blocks[0].label, "test-graph");
        assert_eq!(parsed.meta_blocks[0].format, "dot");
        assert_eq!(parsed.meta_blocks[0].line_count, 2);
        assert!(parsed.meta_blocks[0].readonly);
        assert_eq!(parsed.blocks.len(), 1);
    }

    #[test]
    fn parses_document_with_no_meta_blocks() {
        let input = "#SLOP \"/tmp/file.txt\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 0\nhello";
        let document = parse_document(input).expect("document should parse");
        assert!(document.meta_blocks.is_empty());
        assert_eq!(document.blocks.len(), 1);
    }

    #[test]
    fn rejects_meta_block_with_insufficient_content_lines() {
        let input = "#SLOP_META \"test\" #SLOP_META_KIND codegraph #SLOP_META_FORMAT repomap #SLOP_META_LINES 5 #SLOP_META_READONLY true\nonly_one";
        let error = parse_document(input).expect_err("should fail");
        assert!(error.to_string().contains("exceeds available lines"));
    }
}
