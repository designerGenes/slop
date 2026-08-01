use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::tempdir;

/// `HOME` is isolated so the developer's real `~/.config/slop/config.yaml`
/// cannot flip `verbose_output` or `respect_gitignore` underneath these tests,
/// and `NO_COLOR` keeps the tree assertions free of escape sequences.
fn cargo_bin(home: &Path) -> Command {
    let mut command = Command::cargo_bin("slop").expect("binary should build");
    command
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .env("SLOP_DESLOP_CACHE", "off");
    command
}

fn fixture() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir should exist");
    let root = temp.path().join("project_root");
    fs::create_dir_all(root.join("dir1/dir2/dir3")).expect("dirs should be created");
    fs::write(root.join("dir1/file1.txt"), "one").expect("file should be written");
    fs::write(root.join("dir1/dir2/file2.txt"), "two").expect("file should be written");
    fs::write(root.join("dir1/dir2/dir3/file3.md"), "three").expect("file should be written");
    fs::write(root.join("dir1/dir2/dir3/file4"), "four").expect("file should be written");
    fs::write(root.join("dir1/dir2/dir3/file5.js"), "five").expect("file should be written");
    temp
}

#[test]
fn prints_a_tree_of_the_files_being_slopped() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success()
        .stderr(contains("Slopifying..."))
        .stderr(contains("project_root"))
        .stderr(contains("├── file1.txt").or(contains("└── file1.txt")))
        .stderr(contains("file5.js"))
        .stderr(contains("Slop file written is"))
        .stderr(contains(" kb and was written to:"));
}

#[test]
fn slopignore_prunes_files_without_any_flag() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    fs::write(root.join(".slopignore"), "*.md\n").expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(
        slop.contains("file5.js"),
        "unignored files are still slopped"
    );
    assert!(
        !slop.contains("file3.md"),
        ".slopignore should prune without --respect-gitignore"
    );
}

#[test]
fn verbose_lists_slopignored_paths_and_quiet_mode_does_not() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    fs::write(root.join(".slopignore"), "*.md\n").expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(temp.path().join("out-verbose"))
        .args(["--verbose", "."])
        .assert()
        .success()
        .stderr(contains("file3.md [IGNORED]"));

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(temp.path().join("out-quiet"))
        .arg(".")
        .assert()
        .success()
        .stderr(contains("[IGNORED]").not())
        .stderr(contains("skipped by .slopignore"));
}

#[test]
fn verbose_output_can_be_enabled_from_config() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let config_dir = home.join(".config/slop");
    fs::create_dir_all(&config_dir).expect("config dir should be created");
    fs::write(config_dir.join("config.yaml"), "verbose_output: true\n")
        .expect("config should be written");
    fs::write(root.join(".slopignore"), "*.md\n").expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(temp.path().join("out"))
        .arg(".")
        .assert()
        .success()
        .stderr(contains("file3.md [IGNORED]"));
}

#[test]
fn silent_suppresses_the_run_report() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(temp.path().join("out"))
        .args(["--silent", "."])
        .assert()
        .success()
        .stderr(contains("Slopifying...").not())
        .stderr(contains("Slop file written is").not());
}

#[test]
fn slopignore_supports_negation() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    fs::write(root.join(".slopignore"), "*.txt\n!file1.txt\n")
        .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(slop.contains("file1.txt"), "negated pattern should be kept");
    assert!(!slop.contains("file2.txt"), "*.txt should otherwise prune");
}

#[test]
fn slopinclude_forces_local_and_home_files_without_duplicates() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    let forced = root.join("dir1/dir2/dir3/file3.md");
    let explicit = root.join("dir1/file1.txt");
    let external = home.join("external/file6.md");
    fs::create_dir_all(external.parent().expect("external parent"))
        .expect("external directory should be created");
    fs::write(&external, "external-marker").expect("external file should be written");
    fs::write(
        root.join(".slopignore"),
        "dir1/\n+ dir1/dir2/dir3/file3.md\nslopinclude dir1/dir2/dir3/file3.md\nslopinclude $HOME/external/file6.md\n",
    )
    .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-o"])
        .arg(&output_dir)
        .arg(&explicit)
        .arg(&forced)
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert_eq!(
        slop.matches("three").count(),
        1,
        "forced file is bundled once"
    );
    assert_eq!(
        slop.matches("external-marker").count(),
        1,
        "$HOME include is bundled once"
    );
    assert!(
        slop.contains("one"),
        "the explicitly named file remains in the slop"
    );
}

fn read_only_slop(output_dir: &Path) -> String {
    let entry = fs::read_dir(output_dir)
        .expect("output dir should exist")
        .next()
        .expect("a slop file should have been written")
        .expect("dir entry should be readable");
    fs::read_to_string(entry.path()).expect("slop file should be readable")
}
