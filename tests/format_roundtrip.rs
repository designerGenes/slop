use std::path::PathBuf;

use slop::models::{SoupBlock, SoupPartialRange, SourceFile};
use slop::slop_format::{analyze_contents, compute_block_id, parse_document, serialize_document};

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

fn round_trip(contents: &str) -> String {
    let source = source_file("/tmp/file.txt", contents);
    let serialized = serialize_document(&[], &[source]).expect("document should serialize");
    let parsed = parse_document(&serialized).expect("document should parse");
    let block = &parsed.blocks[0];
    let mut restored = block.content_lines.join("\n");
    if block.trailing_newline {
        restored.push('\n');
    }
    restored
}

#[test]
fn round_trips_empty_file() {
    assert_eq!(round_trip(""), "");
}

#[test]
fn round_trips_one_line_without_trailing_newline() {
    assert_eq!(round_trip("hello"), "hello");
}

#[test]
fn round_trips_one_line_with_trailing_newline() {
    assert_eq!(round_trip("hello\n"), "hello\n");
}

#[test]
fn round_trips_multi_line_file() {
    assert_eq!(round_trip("one\ntwo\nthree"), "one\ntwo\nthree");
}

#[test]
fn round_trips_multi_line_file_ending_with_blank_line() {
    assert_eq!(round_trip("one\ntwo\n\n"), "one\ntwo\n\n");
}

#[test]
fn round_trips_content_with_slop_prefix_lines() {
    assert_eq!(
        round_trip("#SLOP should stay literal\nnext line\n"),
        "#SLOP should stay literal\nnext line\n"
    );
}

#[test]
fn parses_partial_block_metadata() {
    let parsed = parse_document(
        "#SLOP \"/tmp/file.txt\" #SLOP_PARTIAL_LINES 4-5 #SLOPED_LINES 2 #SLOP_TRAILING_NEWLINE 0\nupdated\nlines",
    )
    .expect("document should parse");

    let content_lines = vec!["updated".to_string(), "lines".to_string()];
    assert_eq!(
        parsed.blocks,
        vec![SoupBlock {
            original_absolute_path: PathBuf::from("/tmp/file.txt"),
            partial_range: Some(SoupPartialRange {
                start_line: 4,
                end_line: 5,
            }),
            logical_line_count: 2,
            trailing_newline: false,
            content_lines: content_lines.clone(),
            base_sha: None,
            read_only: false,
            block_id: Some(compute_block_id(&content_lines, false)),
        }]
    );
}
