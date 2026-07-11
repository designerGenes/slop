use std::fs;

use assert_cmd::Command;
use tempfile::tempdir;

fn cargo_bin() -> Command {
    Command::cargo_bin("slop").expect("binary should build")
}

#[test]
fn deslop_restores_deleted_files() {
    let temp = tempdir().expect("tempdir should exist");
    let source = temp.path().join("file1.txt");
    let output_dir = temp.path().join("slop");
    fs::write(&source, "hello\n").expect("source should be written");

    cargo_bin()
        .arg(&source)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    fs::remove_file(&source).expect("source should be removed");

    cargo_bin()
        .args(["-d"])
        .arg(&source)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&source).expect("source should be restored"),
        "hello\n"
    );
}

#[test]
fn deslop_restores_nested_directories() {
    let temp = tempdir().expect("tempdir should exist");
    let folder = temp.path().join("folder1");
    let nested = folder.join("nested/deeper");
    fs::create_dir_all(&nested).expect("directories should be created");
    fs::write(folder.join("file1.md"), "one").expect("file should be written");
    fs::write(nested.join("file2.md"), "two\n").expect("file should be written");
    let output_dir = temp.path().join("slop");

    cargo_bin()
        .arg("-r")
        .arg(&folder)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    fs::remove_dir_all(&folder).expect("folder should be removed");

    cargo_bin()
        .args(["-d"])
        .arg(&folder)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(folder.join("nested/deeper/file2.md")).expect("file should be restored"),
        "two\n"
    );
}

#[test]
fn deslop_overwrites_existing_files() {
    let temp = tempdir().expect("tempdir should exist");
    let file = temp.path().join("file.txt");
    let output_dir = temp.path().join("slop");
    fs::write(&file, "fresh").expect("file should be written");

    cargo_bin()
        .arg(&file)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    fs::write(&file, "stale").expect("file should be overwritten");

    cargo_bin()
        .args(["-d"])
        .arg(&file)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(&file).expect("file should be restored"),
        "fresh"
    );
}

#[test]
fn deslop_works_when_selector_directory_is_missing() {
    let temp = tempdir().expect("tempdir should exist");
    let folder = temp.path().join("folder1");
    let nested = folder.join("nested");
    fs::create_dir_all(&nested).expect("directories should be created");
    fs::write(nested.join("file.txt"), "restored").expect("file should be written");
    let output_dir = temp.path().join("slop");

    cargo_bin()
        .arg("-r")
        .arg(&folder)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    fs::remove_dir_all(&folder).expect("folder should be removed");

    cargo_bin()
        .args(["-d"])
        .arg(&folder)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(folder.join("nested/file.txt")).expect("file should be restored"),
        "restored"
    );
}

#[test]
fn errors_when_no_slop_file_exists() {
    let temp = tempdir().expect("tempdir should exist");
    let output_dir = temp.path().join("slop");
    let selector = temp.path().join("missing.txt");

    cargo_bin()
        .args(["-d"])
        .arg(&selector)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("no matching slop file"));
}

#[test]
fn errors_when_multiple_slop_files_match() {
    let temp = tempdir().expect("tempdir should exist");
    let source = temp.path().join("file.txt");
    let output_dir = temp.path().join("slop");
    fs::write(&source, "hello").expect("source should be written");

    cargo_bin()
        .arg(&source)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    fs::copy(output_dir.join("file.md"), output_dir.join("duplicate.md"))
        .expect("duplicate slop file should be created");

    cargo_bin()
        .args(["-d"])
        .arg(&source)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("multiple slop files matched"));
}

#[cfg(unix)]
#[test]
fn errors_when_restored_file_cannot_be_written() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir should exist");
    let source_dir = temp.path().join("source");
    fs::create_dir_all(&source_dir).expect("directory should be created");
    let source = source_dir.join("file.txt");
    let output_dir = temp.path().join("slop");
    fs::write(&source, "hello").expect("source should be written");

    cargo_bin()
        .arg(&source)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    fs::remove_file(&source).expect("source should be removed");
    fs::set_permissions(&source_dir, fs::Permissions::from_mode(0o500))
        .expect("permissions should be updated");

    cargo_bin()
        .args(["-d"])
        .arg(&source)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains(source.to_string_lossy().as_ref()));

    fs::set_permissions(&source_dir, fs::Permissions::from_mode(0o700))
        .expect("permissions should be restored");
}
