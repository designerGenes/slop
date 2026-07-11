use std::fs;

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

fn cargo_bin() -> Command {
    Command::cargo_bin("slop").expect("binary should build")
}

#[test]
fn deslop_applies_mixed_full_and_partial_blocks() {
    let temp = tempdir().expect("tempdir should exist");
    let full_path = temp.path().join("full.txt");
    let partial_path = temp.path().join("partial.txt");
    let slop_file = temp.path().join("archive.slop");

    fs::write(&partial_path, "one\ntwo\nthree\nfour\n").expect("seed file should be written");
    fs::write(
        &slop_file,
        format!(
            concat!(
                "#SLOP \"{}\" #SLOPED_LINES 2 #SLOP_TRAILING_NEWLINE 1\n",
                "fresh\nfile\n",
                "#SLOP \"{}\" #SLOP_PARTIAL_LINES 2-3 #SLOPED_LINES 2 #SLOP_TRAILING_NEWLINE 1\n",
                "dos\nthree updated"
            ),
            full_path.display(),
            partial_path.display()
        ),
    )
    .expect("slop file should be written");

    cargo_bin().args(["-d"]).arg(&slop_file).assert().success();

    assert_eq!(
        fs::read_to_string(&full_path).expect("full file should be restored"),
        "fresh\nfile\n"
    );
    assert_eq!(
        fs::read_to_string(&partial_path).expect("partial file should be updated"),
        "one\ndos\nthree updated\nfour\n"
    );
}

#[test]
fn deslop_applies_multiple_partial_blocks_in_order() {
    let temp = tempdir().expect("tempdir should exist");
    let path = temp.path().join("ordered.txt");
    let slop_file = temp.path().join("ordered.slop");

    fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").expect("seed file should be written");
    fs::write(
        &slop_file,
        format!(
            concat!(
                "#SLOP \"{}\" #SLOP_PARTIAL_LINES 2-2 #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1\n",
                "beta updated\n",
                "#SLOP \"{}\" #SLOP_PARTIAL_LINES 3-4 #SLOPED_LINES 2 #SLOP_TRAILING_NEWLINE 0\n",
                "gamma updated\nomega"
            ),
            path.display(),
            path.display()
        ),
    )
    .expect("slop file should be written");

    cargo_bin().args(["-d"]).arg(&slop_file).assert().success();

    assert_eq!(
        fs::read_to_string(&path).expect("file should be updated"),
        "alpha\nbeta updated\ngamma updated\nomega"
    );
}

#[test]
fn deslop_reports_partial_ranges_that_exceed_existing_file_length() {
    let temp = tempdir().expect("tempdir should exist");
    let path = temp.path().join("short.txt");
    let slop_file = temp.path().join("broken.slop");

    fs::write(&path, "one\ntwo\n").expect("seed file should be written");
    fs::write(
        &slop_file,
        format!(
            "#SLOP \"{}\" #SLOP_PARTIAL_LINES 2-4 #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1\nreplaced",
            path.display()
        ),
    )
    .expect("slop file should be written");

    cargo_bin()
        .args(["-d"])
        .arg(&slop_file)
        .assert()
        .failure()
        .stderr(contains("partial slop range 2-4 exceeds existing file length 2"));
}

#[test]
fn deslop_applies_partial_block_despite_base_sha_drift() {
    let temp = tempdir().expect("tempdir should exist");
    let path = temp.path().join("drifted.txt");
    let slop_file = temp.path().join("round2.slop");

    fs::write(&path, "round1\nseed\ncontent\n").expect("post-round-1 file should be written");

    let stale_sha = "0".repeat(64);
    fs::write(
        &slop_file,
        format!(
            "#SLOP \"{}\" #SLOP_PARTIAL_LINES 2-2 #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1 #SLOP_BASE_SHA {}\nupdated line two",
            path.display(),
            stale_sha
        ),
    )
    .expect("slop file should be written");

    cargo_bin()
        .args(["-d"])
        .arg(&slop_file)
        .assert()
        .success()
        .stderr(contains("base SHA drift"));

    assert_eq!(
        fs::read_to_string(&path).expect("partial block should still be applied"),
        "round1\nupdated line two\ncontent\n"
    );
}

#[test]
fn deslop_partial_block_is_idempotent_across_runs() {
    let temp = tempdir().expect("tempdir should exist");
    let path = temp.path().join("source.md");
    let slop_file = temp.path().join("changes.slop");

    fs::write(&path, "line1\nline2\nline3\nline4\n").expect("seed file should be written");
    fs::write(
        &slop_file,
        format!(
            concat!(
                "#SLOP \"{}\" #SLOP_PARTIAL_LINES 2-3 #SLOPED_LINES 3 #SLOP_TRAILING_NEWLINE 1\n",
                "line2 changed\nline3 changed\nline 3.1 new line!"
            ),
            path.display()
        ),
    )
    .expect("slop file should be written");

    let expected = "line1\nline2 changed\nline3 changed\nline 3.1 new line!\nline4\n";

    cargo_bin()
        .args(["-d"])
        .arg(&slop_file)
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&path).expect("file should be updated after first run"),
        expected
    );

    // Forgetfully re-run deslop against the same slop document.
    cargo_bin()
        .args(["-d"])
        .arg(&slop_file)
        .assert()
        .success()
        .stderr(contains("already applied"));

    assert_eq!(
        fs::read_to_string(&path).expect("file should be unchanged after second run"),
        expected,
        "second deslop run must not corrupt the already-updated file"
    );
}
