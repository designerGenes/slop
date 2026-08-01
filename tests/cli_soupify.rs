use std::fs;
use std::path::PathBuf;

use assert_cmd::Command;
use tempfile::tempdir;

fn cargo_bin() -> Command {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_HOME: AtomicUsize = AtomicUsize::new(0);
    let home = std::env::temp_dir().join(format!(
        "slop-cli-test-{}-{}",
        std::process::id(),
        NEXT_HOME.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&home).expect("test home should be created");
    let mut command = Command::cargo_bin("slop").expect("binary should build");
    command.env("HOME", home).env("SLOP_DESLOP_CACHE", "off");
    command
}

#[test]
fn slopifies_one_file() {
    let temp = tempdir().expect("tempdir should exist");
    let input = temp.path().join("file1.md");
    let output_dir = temp.path().join("out");
    fs::write(&input, "hello world\n").expect("input file should be written");

    cargo_bin()
        .arg(&input)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    let slop = fs::read_to_string(output_dir.join("file1.md")).expect("slop file should exist");
    assert!(slop.contains("#SLOP"));
    assert!(slop.contains(input.to_string_lossy().as_ref()));
}

#[test]
fn slopifies_multiple_files() {
    let temp = tempdir().expect("tempdir should exist");
    let file1 = temp.path().join("file1.md");
    let file3 = temp.path().join("file3.md");
    let output_dir = temp.path().join("out");
    fs::write(&file1, "one").expect("input file should be written");
    fs::write(&file3, "three").expect("input file should be written");

    cargo_bin()
        .args([file3.as_os_str(), file1.as_os_str()])
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    assert!(output_dir.join("file1_file3.md").exists());
}

#[test]
fn slopifies_nested_directories() {
    let temp = tempdir().expect("tempdir should exist");
    let directory = temp.path().join("folder1");
    fs::create_dir_all(directory.join("nested/deeper")).expect("directories should be created");
    fs::write(directory.join("file1.md"), "one").expect("input file should be written");
    fs::write(directory.join("nested/file2.md"), "two").expect("input file should be written");
    fs::write(directory.join("nested/deeper/file4.md"), "four")
        .expect("input file should be written");
    let output_dir = temp.path().join("out");

    cargo_bin()
        .arg("-r")
        .arg(&directory)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    assert!(output_dir.join("file1_file2_file4.md").exists());
}

#[test]
fn slopifies_directory_without_recursive_flag() {
    let temp = tempdir().expect("tempdir should exist");
    let directory = temp.path().join("folder1");
    fs::create_dir_all(&directory).expect("directory should be created");
    fs::create_dir_all(directory.join("subdir")).expect("subdir should be created");
    fs::write(directory.join("file1.md"), "one").expect("input file should be written");
    fs::write(directory.join("subdir/nested.md"), "two").expect("nested file should be written");
    let output_dir = temp.path().join("out");

    cargo_bin()
        .arg(&directory)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    // Should only include file1.md, not nested.md from subdir
    assert!(output_dir.join("file1.md").exists());
    assert!(!output_dir.join("nested.md").exists());
}

#[test]
fn slopifies_only_direct_files_without_recursive_flag() {
    let temp = tempdir().expect("tempdir should exist");
    let file1 = temp.path().join("file1.md");
    let file2 = temp.path().join("file2.md");
    let nested_dir = temp.path().join("nested");
    fs::create_dir_all(&nested_dir).expect("nested directory should be created");
    fs::write(&file1, "one").expect("file should be written");
    fs::write(&file2, "two").expect("file should be written");
    fs::write(nested_dir.join("file3.md"), "three").expect("nested file should be written");
    let output_dir = temp.path().join("out");

    cargo_bin()
        .arg(&file1)
        .arg(&file2)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    let slop = fs::read_to_string(output_dir.join("file1_file2.md"))
        .expect("slop file should exist");
    assert!(!slop.contains("three")); // nested file should not be included
}

#[test]
fn uses_default_output_directory() {
    let temp = tempdir().expect("tempdir should exist");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("home should be created");
    let input = temp.path().join("sample.txt");
    fs::write(&input, "sample").expect("input file should be written");

    cargo_bin()
        .env("HOME", &home)
        .arg(&input)
        .assert()
        .success();

    assert!(home.join(".slop/slopified/sample.md").exists());
}

#[test]
fn supports_output_directory_override() {
    let temp = tempdir().expect("tempdir should exist");
    let input = temp.path().join("sample.txt");
    let output_dir = temp.path().join("custom-output");
    fs::write(&input, "sample").expect("input file should be written");

    cargo_bin()
        .arg(&input)
        .arg("--output")
        .arg(&output_dir)
        .assert()
        .success();

    assert!(output_dir.join("sample.md").exists());
}

#[test]
fn reports_output_directory_creation_failure() {
    let temp = tempdir().expect("tempdir should exist");
    let input = temp.path().join("sample.txt");
    let occupied_path = temp.path().join("not-a-directory");
    fs::write(&input, "sample").expect("input file should be written");
    fs::write(&occupied_path, "occupied").expect("blocking file should be written");

    cargo_bin()
        .arg(&input)
        .arg("-o")
        .arg(&occupied_path)
        .assert()
        .failure()
        .stderr(predicates::str::contains("failed to create directory"))
        .stderr(predicates::str::contains(
            occupied_path.to_string_lossy().as_ref(),
        ));
}

#[test]
fn generates_expected_output_filename() {
    let temp = tempdir().expect("tempdir should exist");
    let file1 = temp.path().join("file1.md");
    let env_file = temp.path().join(".env");
    let gitignore = temp.path().join(".gitignore");
    let output_dir = temp.path().join("out");
    fs::write(&file1, "one").expect("input file should be written");
    fs::write(&env_file, "two").expect("input file should be written");
    fs::write(&gitignore, "three").expect("input file should be written");

    cargo_bin()
        .args([
            gitignore.as_os_str(),
            env_file.as_os_str(),
            file1.as_os_str(),
        ])
        .arg("--allow-secrets")
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    assert!(output_dir.join("env_file1_gitignore.md").exists());
}

#[test]
fn overwrites_existing_slop_file() {
    let temp = tempdir().expect("tempdir should exist");
    let input = temp.path().join("sample.txt");
    let output_dir = temp.path().join("out");
    fs::create_dir_all(&output_dir).expect("output dir should be created");
    fs::write(&input, "updated").expect("input file should be written");
    fs::write(output_dir.join("sample.md"), "old contents")
        .expect("existing slop file should be written");

    cargo_bin()
        .arg(&input)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    let slop = fs::read_to_string(output_dir.join("sample.md")).expect("slop file should exist");
    assert!(slop.contains("updated"));
    assert!(!slop.contains("old contents"));
}

#[test]
fn invokes_show_output_directory_via_mock_abstraction() {
    let temp = tempdir().expect("tempdir should exist");
    let input = temp.path().join("sample.txt");
    let output_dir = temp.path().join("out");
    let mock_log = temp.path().join("open.log");
    fs::write(&input, "sample").expect("input file should be written");

    cargo_bin()
        .env("slop_OPEN_MOCK_FILE", &mock_log)
        .arg(&input)
        .arg("-o")
        .arg(&output_dir)
        .arg("-s")
        .assert()
        .success();

    let logged = fs::read_to_string(&mock_log).expect("mock log should be written");
    assert_eq!(logged.trim(), output_dir.to_string_lossy());
}

#[test]
fn reports_open_failure_after_writing_file() {
    let temp = tempdir().expect("tempdir should exist");
    let input = temp.path().join("sample.txt");
    let output_dir = temp.path().join("out");
    fs::write(&input, "sample").expect("input file should be written");

    cargo_bin()
        .env("slop_OPEN_FORCE_FAIL", "1")
        .arg(&input)
        .arg("-o")
        .arg(&output_dir)
        .arg("-s")
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            output_dir.join("sample.md").to_string_lossy().as_ref(),
        ))
        .stderr(predicates::str::contains("forced open failure"));

    assert!(output_dir.join("sample.md").exists());
}

#[test]
fn rejects_non_utf8_files() {
    let temp = tempdir().expect("tempdir should exist");
    let input = temp.path().join("binary.txt");
    let output_dir = temp.path().join("out");
    fs::write(&input, [0xff_u8, 0xfe_u8]).expect("bytes should be written");

    cargo_bin()
        .arg(&input)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("not valid UTF-8"));
}

#[test]
fn rejects_missing_input_paths() {
    let temp = tempdir().expect("tempdir should exist");
    let missing = temp.path().join("missing.txt");

    cargo_bin()
        .arg(&missing)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            missing.to_string_lossy().as_ref(),
        ));
}

#[test]
fn rejects_deslop_show_combination() {
    cargo_bin()
        .args(["-d", "-s", "input.txt"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be combined"));
}

#[test]
fn rejects_input_that_expands_to_zero_files() {
    let temp = tempdir().expect("tempdir should exist");
    let empty_dir = temp.path().join("empty");
    fs::create_dir_all(&empty_dir).expect("directory should be created");
    let output_dir = temp.path().join("out");

    cargo_bin()
        .arg("-r")
        .arg(&empty_dir)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("expanded to zero files"));
}

#[test]
fn rejects_malformed_existing_slop_during_deslop() {
    let temp = tempdir().expect("tempdir should exist");
    let slop_dir = temp.path().join("slop");
    fs::create_dir_all(&slop_dir).expect("directory should be created");
    let selector = temp.path().join("file.txt");
    fs::write(slop_dir.join("bad.md"), "#SLOP \"/tmp/file.txt\"")
        .expect("slop file should be written");

    cargo_bin()
        .args(["-d"])
        .arg(&selector)
        .arg("-o")
        .arg(&slop_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("malformed slop header"));
}

#[test]
fn rejects_slop_missing_required_metadata() {
    let temp = tempdir().expect("tempdir should exist");
    let slop_dir = temp.path().join("slop");
    fs::create_dir_all(&slop_dir).expect("directory should be created");
    let selector = PathBuf::from("/tmp/file.txt");
    fs::write(
        slop_dir.join("bad.md"),
        format!("#SLOP \"{}\" #SLOPED_LINES 1\nhello", selector.display()),
    )
    .expect("slop file should be written");

    cargo_bin()
        .args(["-d"])
        .arg(&selector)
        .arg("-o")
        .arg(&slop_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("missing slop metadata"));
}

#[test]
fn rejects_declared_line_count_exceeding_available_lines() {
    let temp = tempdir().expect("tempdir should exist");
    let slop_dir = temp.path().join("slop");
    fs::create_dir_all(&slop_dir).expect("directory should be created");
    let selector = PathBuf::from("/tmp/file.txt");
    fs::write(
        slop_dir.join("bad.md"),
        format!(
            "#SLOP \"{}\" #SLOPED_LINES 2 #SLOP_TRAILING_NEWLINE 0\nhello",
            selector.display()
        ),
    )
    .expect("slop file should be written");

    cargo_bin()
        .args(["-d"])
        .arg(&selector)
        .arg("-o")
        .arg(&slop_dir)
        .assert()
        .failure()
    .stderr(predicates::str::contains(
        "declared line count 2 exceeds available lines",
    ));
}

#[test]
fn excludes_files_by_pattern() {
    let temp = tempdir().expect("tempdir should exist");
    let file1 = temp.path().join("file1.md");
    let file2 = temp.path().join("file2.swift");
    let file3 = temp.path().join("file3.md");
    let output_dir = temp.path().join("out");
    fs::write(&file1, "one").expect("file should be written");
    fs::write(&file2, "two").expect("file should be written");
    fs::write(&file3, "three").expect("file should be written");

    cargo_bin()
        .args([file1.as_os_str(), file2.as_os_str(), file3.as_os_str()])
        .arg("--exclude")
        .arg("*.swift")
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    let slop = fs::read_to_string(output_dir.join("file1_file3.md"))
        .expect("slop file should exist");
    assert!(slop.contains("one"));
    assert!(slop.contains("three"));
    assert!(!slop.contains("two")); // swift file should be excluded
}

#[test]
fn excludes_files_by_folder_name() {
    let temp = tempdir().expect("tempdir should exist");
    let folder1 = temp.path().join("folder1");
    let folder2 = temp.path().join("folder2");
    fs::create_dir_all(&folder1).expect("folder should be created");
    fs::create_dir_all(&folder2).expect("folder should be created");
    fs::write(folder1.join("file1.md"), "one").expect("file should be written");
    fs::write(folder2.join("file2.md"), "two").expect("file should be written");
    let output_dir = temp.path().join("out");

    cargo_bin()
        .arg("-r")
        .arg(folder1.parent().unwrap())
        .arg("--exclude")
        .arg("folder2")
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    let slop = fs::read_to_string(output_dir.join("file1.md"))
        .expect("slop file should exist");
    assert!(slop.contains("one"));
    assert!(!slop.contains("two")); // folder2 file should be excluded
}

#[test]
fn excludes_files_by_regex() {
    let temp = tempdir().expect("tempdir should exist");
    let file1 = temp.path().join("file1.md");
    let file2 = temp.path().join("test_file.md");
    let file3 = temp.path().join("example.md");
    let output_dir = temp.path().join("out");
    fs::write(&file1, "one").expect("file should be written");
    fs::write(&file2, "two").expect("file should be written");
    fs::write(&file3, "three").expect("file should be written");

    cargo_bin()
        .args([file1.as_os_str(), file2.as_os_str(), file3.as_os_str()])
        .arg("--exclude")
        .arg("/^test_/")
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    // Files are sorted alphabetically, so the output filename will be example_file1.md
    let slop = fs::read_to_string(output_dir.join("example_file1.md"))
        .expect("slop file should exist");
    assert!(slop.contains("one"));
    assert!(slop.contains("three"));
    assert!(!slop.contains("two")); // test_file should be excluded
}

#[test]
fn excludes_dotfiles() {
    let temp = tempdir().expect("tempdir should exist");
    let file1 = temp.path().join("file1.md");
    let file2 = temp.path().join(".env");
    let output_dir = temp.path().join("out");
    fs::write(&file1, "one").expect("file should be written");
    fs::write(&file2, "two").expect("file should be written");

    cargo_bin()
        .args([file1.as_os_str(), file2.as_os_str()])
        .arg("--exclude")
        .arg(".*")
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    assert!(output_dir.join("file1.md").exists());
    assert!(!output_dir.join("env_file1.md").exists()); // Should not have env in name
}

#[test]
fn excludes_all_files_results_in_zero_files_error() {
    let temp = tempdir().expect("tempdir should exist");
    let file1 = temp.path().join("file1.md");
    let output_dir = temp.path().join("out");
    fs::write(&file1, "one").expect("file should be written");

    cargo_bin()
        .arg(&file1)
        .arg("--exclude")
        .arg("*.md")
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .failure()
        .stderr(predicates::str::contains("expanded to zero files"));
}

#[test]
fn respects_gitignore_when_flag_passed() {
    let temp = tempdir().expect("tempdir should exist");
    let directory = temp.path().join("folder1");
    fs::create_dir_all(&directory).expect("directory should be created");
    fs::write(directory.join(".gitignore"), "ignored.md\n")
        .expect("gitignore should be written");
    fs::write(directory.join("file1.md"), "one").expect("file should be written");
    fs::write(directory.join("ignored.md"), "two").expect("file should be written");
    let output_dir = temp.path().join("out");

    cargo_bin()
        .arg("-r")
        .arg("--respect-gitignore")
        .arg(&directory)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    // file1.md + the .gitignore file itself (dotfiles still slop by
    // default); ignored.md is pruned because .gitignore lists it.
    let slop = fs::read_to_string(output_dir.join("file1_gitignore.md"))
        .expect("slop file should exist");
    assert!(slop.contains("one"));
    assert!(!slop.contains("two"));
}

#[test]
fn ignores_gitignore_contents_without_flag() {
    let temp = tempdir().expect("tempdir should exist");
    let directory = temp.path().join("folder1");
    fs::create_dir_all(&directory).expect("directory should be created");
    fs::write(directory.join(".gitignore"), "ignored.md\n")
        .expect("gitignore should be written");
    fs::write(directory.join("file1.md"), "one").expect("file should be written");
    fs::write(directory.join("ignored.md"), "two").expect("file should be written");
    let output_dir = temp.path().join("out");

    cargo_bin()
        .arg("-r")
        .arg(&directory)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    // Without --respect-gitignore, .gitignore is just another file - nothing
    // is pruned, matching existing (pre-flag) behavior.
    let slop = fs::read_to_string(output_dir.join("file1_gitignore_ignored.md"))
        .expect("slop file should exist");
    assert!(slop.contains("one"));
    assert!(slop.contains("two"));
}

#[test]
fn respect_gitignore_config_default_enables_pruning_without_flag() {
    let temp = tempdir().expect("tempdir should exist");
    let home = temp.path().join("home");
    fs::create_dir_all(home.join(".config/slop")).expect("config dir should be created");
    fs::write(
        home.join(".config/slop/config.yaml"),
        "respect_gitignore: true\n",
    )
    .expect("config should be written");

    let directory = temp.path().join("folder1");
    fs::create_dir_all(&directory).expect("directory should be created");
    fs::write(directory.join(".gitignore"), "ignored.md\n")
        .expect("gitignore should be written");
    fs::write(directory.join("file1.md"), "one").expect("file should be written");
    fs::write(directory.join("ignored.md"), "two").expect("file should be written");
    let output_dir = temp.path().join("out");

    cargo_bin()
        .env("HOME", &home)
        .arg("-r")
        .arg(&directory)
        .arg("-o")
        .arg(&output_dir)
        .assert()
        .success();

    let slop = fs::read_to_string(output_dir.join("file1_gitignore.md"))
        .expect("slop file should exist");
    assert!(slop.contains("one"));
    assert!(!slop.contains("two"));
}
