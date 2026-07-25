use std::fs;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

fn cargo_bin() -> Command {
    let mut cmd = Command::cargo_bin("slop").expect("binary should build");
    cmd.env("SLOP_DESLOP_CACHE", "off");
    cmd
}

#[test]
fn deslop_accepts_a_direct_slop_file_path() {
    let temp = tempdir().expect("tempdir should exist");
    let restored = temp.path().join("nested/file.txt");
    let slop_file = temp.path().join("archive.slop");
    fs::write(
        &slop_file,
        format!(
            "#SLOP \"{}\" #SLOPED_LINES 2 #SLOP_TRAILING_NEWLINE 1\nhello\nworld",
            restored.display()
        ),
    )
    .expect("slop file should be written");

    cargo_bin().args(["-d"]).arg(&slop_file).assert().success();

    assert_eq!(
        fs::read_to_string(&restored).expect("restored file should exist"),
        "hello\nworld\n"
    );
}

#[test]
fn direct_slop_path_reports_parse_errors_from_that_file() {
    let temp = tempdir().expect("tempdir should exist");
    let slop_file = temp.path().join("broken.slop");
    fs::write(&slop_file, "#SLOP \"/tmp/file.txt\"").expect("slop file should be written");

    cargo_bin()
        .args(["-d"])
        .arg(&slop_file)
        .assert()
        .failure()
        .stderr(contains("malformed slop header"));
}

#[test]
fn deslop_accepts_a_txt_file_with_slop_content() {
    let temp = tempdir().expect("tempdir should exist");
    let restored = temp.path().join("out/hello.txt");
    let slop_file = temp.path().join("notes.txt");
    fs::write(
        &slop_file,
        format!(
            "#SLOP \"{}\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1\nhello",
            restored.display()
        ),
    )
    .expect("slop file should be written");

    cargo_bin().args(["-d"]).arg(&slop_file).assert().success();

    assert_eq!(
        fs::read_to_string(&restored).expect("restored file should exist"),
        "hello\n"
    );
}
