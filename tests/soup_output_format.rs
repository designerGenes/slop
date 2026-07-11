use std::fs;

use assert_cmd::Command;
use tempfile::tempdir;

fn cargo_bin() -> Command {
    Command::cargo_bin("slop").expect("binary should build")
}

#[test]
fn slop_output_is_markdown_even_for_non_markdown_inputs() {
    let temp = tempdir().expect("tempdir should exist");
    let input = temp.path().join("notes.txt");
    let output_dir = temp.path().join("out");
    fs::write(&input, "hello world").expect("input file should be written");

    cargo_bin()
        .arg(&input)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    let slop_path = output_dir.join("notes.md");
    assert!(slop_path.exists());
    assert_eq!(
        slop_path.extension().and_then(|extension| extension.to_str()),
        Some("md")
    );
}
