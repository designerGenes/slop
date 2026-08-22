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
    let external = home.join("external");
    let external_file = external.join("file6.md");
    let nested_external_file = external.join("nested/file7.md");
    fs::create_dir_all(nested_external_file.parent().expect("external parent"))
        .expect("external directory should be created");
    fs::write(&external_file, "external-marker").expect("external file should be written");
    fs::write(&nested_external_file, "nested-directory-marker")
        .expect("nested external file should be written");
    fs::write(
        root.join(".slopignore"),
        "dir1/\n+ dir1/dir2/dir3/file3.md\nslopinclude dir1/dir2/dir3/file3.md\nslopinclude $HOME/external/\n",
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
    assert_eq!(
        slop.matches("nested-directory-marker").count(),
        1,
        "$HOME directory includes nested files"
    );
    assert!(
        slop.contains("one"),
        "the explicitly named file remains in the slop"
    );
}

#[test]
fn config_skips_slopignore_for_an_explicit_file_statement() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    let config_dir = home.join(".config/slop");
    fs::create_dir_all(&config_dir).expect("config dir should be created");
    fs::write(
        config_dir.join("config.yaml"),
        "skip_slopignore_for_full_statement: true\n",
    )
    .expect("config should be written");
    fs::write(root.join("extra.txt"), "should not be added").expect("file should be written");
    fs::write(root.join(".slopignore"), "*\nslopinclude extra.txt\n")
        .expect("slopignore should be written");
    let explicit = root.join("dir1/file1.txt");
    let second_explicit = root.join("dir1/dir2/dir3/file3.md");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-o"])
        .arg(&output_dir)
        .arg(&explicit)
        .arg(&second_explicit)
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(slop.contains("one"));
    assert!(slop.contains("three"));
    assert!(
        !slop.contains("should not be added"),
        "full explicit-file statements must not execute slopinclude directives"
    );
}

#[test]
fn explicit_file_inputs_override_slopignore_when_config_is_false() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    fs::write(root.join(".slopignore"), "*\n").expect("slopignore should be written");
    let explicit = root.join("dir1/dir2/dir3/file3.md");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-o"])
        .arg(&output_dir)
        .arg(&explicit)
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(
        slop.contains("three"),
        "an explicitly named file must bypass .slopignore even when the config setting is false"
    );
}

#[test]
fn ignore_slopignore_flag_bypasses_rules_for_directory_walks() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    let external = home.join("external/file6.md");
    fs::create_dir_all(external.parent().expect("external parent"))
        .expect("external directory should be created");
    fs::write(&external, "must not be added").expect("external file should be written");
    fs::write(
        root.join(".slopignore"),
        "*\nslopinclude $HOME/external/\n",
    )
    .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "--ignore-slopignore", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(
        slop.contains("three"),
        "--ignore-slopignore must bypass ignore rules even for directory walks"
    );
    assert!(
        !slop.contains("must not be added"),
        "--ignore-slopignore must not execute slopinclude directives"
    );
}

#[test]
fn run_report_shows_external_slopinclude_files() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    let external = home.join("external/file6.md");
    fs::create_dir_all(external.parent().expect("external parent"))
        .expect("external directory should be created");
    fs::write(&external, "external-marker").expect("external file should be written");
    fs::write(
        root.join(".slopignore"),
        "slopinclude $HOME/external/\n",
    )
    .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success()
        .stderr(contains("[external]"))
        .stderr(contains("file6.md"));
}

#[test]
fn subfolder_invocation_uses_own_slopignore_not_parents() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let subfolder = root.join("dir1/dir2");
    let output_dir = temp.path().join("out");

    // The parent's directives must be invisible from the subfolder: its ignore
    // rule would prune a file inside the calling folder, and its slopinclude
    // targets a file OUTSIDE it.
    fs::write(root.join(".slopignore"), "*.txt\nslopinclude dir1/file1.txt\n")
        .expect("root slopignore should be written");
    // The subfolder has a .slopignore of its own: it alone governs the walk.
    fs::write(subfolder.join(".slopignore"), "*.js\n")
        .expect("subfolder slopignore should be written");

    cargo_bin(&home)
        .current_dir(&subfolder)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(
        slop.contains("two"),
        "file2.txt must not be pruned by the parent .slopignore"
    );
    assert!(
        slop.contains("three"),
        "file3.md lives beneath the calling folder and is slopped"
    );
    assert!(
        !slop.contains("five"),
        "the subfolder's own .slopignore prunes file5.js"
    );
    assert!(
        !slop.contains("one"),
        "the parent .slopignore's slopinclude must not execute from a subfolder invocation"
    );
}

#[test]
fn slopignore_files_are_automatically_ignored_by_default() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    fs::write(root.join(".slopignore"), "*.log\n").expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(
        !slop.contains(".slopignore"),
        ".slopignore should be automatically ignored from the slop output bundle by default"
    );
}

#[test]
fn slopignore_is_included_when_explicitly_slopincluded() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    fs::write(root.join(".slopignore"), "*.log\n+ .slopignore\n").expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(
        slop.contains(".slopignore"),
        ".slopignore should be included when explicitly named in a slopinclude directive"
    );
}

#[test]
fn subfolder_slopignore_files_are_ignored_by_default_and_during_directory_includes() {
    let temp = fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let subfolder = root.join("sub1");
    let output_dir = temp.path().join("out");

    fs::create_dir_all(&subfolder).expect("subfolder should exist");
    fs::write(root.join(".slopignore"), "+ sub1\n").expect("root slopignore");
    fs::write(subfolder.join(".slopignore"), "*.tmp\n").expect("subfolder slopignore");
    fs::write(subfolder.join("file3.md"), "content").expect("file3 should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(
        slop.contains("file3.md"),
        "sub1/file3.md should be included"
    );
    assert!(
        !slop.contains("sub1/.slopignore"),
        "sub1/.slopignore must be automatically ignored even when sub1 is included"
    );
}

#[test]
fn current_directory_without_slopignore_uses_manifest_shallow_walk() {
    let temp = tempdir().expect("tempdir should exist");
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");
    fs::create_dir_all(root.join("child/grandchild")).expect("dirs should be created");
    fs::write(root.join("top.txt"), "top-level").expect("top file should be written");
    fs::write(root.join("child/near.txt"), "immediate-child")
        .expect("near file should be written");
    fs::write(root.join("child/grandchild/deep.txt"), "too-deep")
        .expect("deep file should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(slop.contains("top-level"));
    assert!(slop.contains("immediate-child"));
    assert!(
        !slop.contains("too-deep"),
        "a missing .slopignore must not turn slop . into a recursive walk"
    );
}

#[test]
fn manifest_resolves_include_folder_and_cli_exclude_precedence() {
    let temp = tempdir().expect("tempdir should exist");
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let included_dir = root.join("someFolder");
    fs::create_dir_all(&included_dir).expect("included directory should exist");
    fs::write(root.join("baseline.txt"), "baseline").expect("baseline file should be written");
    fs::write(included_dir.join("kept.py"), "included-python")
        .expect("python file should be written");
    fs::write(included_dir.join("ignored.txt"), "ignored-folder-file")
        .expect("folder file should be written");
    fs::write(root.join(".slopignore"), "someFolder/\n+ *.py\n")
        .expect("slopignore should be written");

    let include_output = temp.path().join("include-out");
    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(&include_output)
        .arg(".")
        .assert()
        .success();

    let included = read_only_slop(&include_output);
    assert!(
        included.contains("included-python"),
        "+ *.py must override the someFolder/ ignore rule"
    );
    assert!(!included.contains("ignored-folder-file"));

    let excluded_output = temp.path().join("excluded-out");
    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-x", "*.py", "-o"])
        .arg(&excluded_output)
        .arg(".")
        .assert()
        .success();

    let excluded = read_only_slop(&excluded_output);
    assert!(excluded.contains("baseline"));
    assert!(
        !excluded.contains("included-python"),
        "-x *.py must override the + *.py include directive"
    );
}

#[test]
fn slopheaps_include_other_roots_with_local_exclusions_and_nesting() {
    let temp = tempdir().expect("tempdir should exist");
    let home = temp.path().join("home");
    let pocket = temp.path().join("pocket");
    let heap = temp.path().join("project");
    let nested_heap = temp.path().join("nested-project");
    let sibling_heap = temp.path().join("sibling-project");
    let output_dir = temp.path().join("out");

    fs::create_dir_all(&pocket).expect("pocket should exist");
    fs::create_dir_all(heap.join("src")).expect("heap source dir should exist");
    fs::create_dir_all(&nested_heap).expect("nested heap should exist");
    fs::create_dir_all(&sibling_heap).expect("sibling heap should exist");
    fs::write(pocket.join("pocket.txt"), "pocket-marker").expect("pocket file");
    fs::write(heap.join("src/keep.rs"), "heap-source-marker").expect("heap source file");
    fs::write(heap.join("src/excluded.rs"), "heap-excluded-marker")
        .expect("excluded source file");
    fs::write(heap.join("tool.py"), "heap-python-marker").expect("heap python file");
    fs::write(heap.join("other.txt"), "heap-unselected-marker").expect("heap other file");
    fs::write(nested_heap.join("README.md"), "nested-heap-marker").expect("nested readme");
    fs::write(sibling_heap.join("sibling.txt"), "sibling-heap-marker")
        .expect("sibling file");

    fs::write(
        pocket.join(".slopignore"),
        format!(
            "*\n\\/ {}\n  + src/\n  + *.py\n  src/excluded.rs\n  \\/ {}\n    + README.md\n  /\\\n/\\\n\\/ {}\n  + sibling.txt\n/\\\n",
            heap.display(),
            nested_heap.display(),
            sibling_heap.display(),
        ),
    )
    .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&pocket)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(slop.contains("heap-source-marker"));
    assert!(slop.contains("heap-python-marker"));
    assert!(slop.contains("nested-heap-marker"));
    assert!(slop.contains("sibling-heap-marker"));
    assert!(!slop.contains("pocket-marker"));
    assert!(!slop.contains("heap-excluded-marker"));
    assert!(!slop.contains("heap-unselected-marker"));
}

#[test]
fn slopheap_options_apply_to_the_targeted_root() {
    let temp = tempdir().expect("tempdir should exist");
    let home = temp.path().join("home");
    let pocket = temp.path().join("pocket");
    let heap = temp.path().join("project");
    let output_dir = temp.path().join("out");

    fs::create_dir_all(&pocket).expect("pocket should exist");
    fs::create_dir_all(heap.join("src")).expect("heap source dir should exist");
    fs::create_dir_all(heap.join("lib")).expect("heap library dir should exist");
    fs::write(
        heap.join("src/seed.rs"),
        "pub fn use_helper() { helper(); }\n",
    )
    .expect("seed source file");
    fs::write(
        heap.join("src/excluded.rs"),
        "pub fn excluded_marker() {}\n",
    )
    .expect("excluded source file");
    fs::write(heap.join("lib/helper.rs"), "pub fn helper() {}\n")
        .expect("graph-only source file");
    fs::write(heap.join("CONTEXT.md"), "heap-context-marker\n")
        .expect("context file");
    fs::write(
        heap.join("src/secret.txt"),
        "AWS_ACCESS_KEY_ID=AKIA1234567890123456\n",
    )
    .expect("secret fixture");
    fs::write(heap.join("src/gitignored.rs"), "pub fn gitignored() {}\n")
        .expect("gitignored fixture");
    fs::write(heap.join(".gitignore"), "src/gitignored.rs\n").expect("heap gitignore");
    fs::write(
        pocket.join(".slopignore"),
        format!(
            "*\n\\/ {}\n  + src/\n  + CONTEXT.md\n/\\ -g --graph-map-tokens 4096 -x excluded.rs --context-file CONTEXT.md --redact --respect-gitignore --seed src/gitignored.rs\n",
            heap.display()
        ),
    )
    .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&pocket)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    let document = slop::slop_format::parse_document(&slop).expect("generated slop should parse");
    assert_eq!(document.meta_blocks.len(), 1);
    assert_eq!(document.meta_blocks[0].label, "repo-graph");
    assert!(
        document.meta_blocks[0]
            .content_lines
            .iter()
            .any(|line| line.contains("lib/helper.rs")),
        "the graph must be generated from the heap root rather than the pocket"
    );
    assert!(slop.contains("use_helper"));
    assert!(
        document
            .blocks
            .iter()
            .all(|block| block.original_absolute_path != heap.join("src/excluded.rs")),
        "heap -x options must exclude matching source blocks"
    );
    assert!(
        document
            .blocks
            .iter()
            .all(|block| block.original_absolute_path != heap.join("src/gitignored.rs")),
        "heap --respect-gitignore must prune matching source blocks"
    );
    let context = document
        .blocks
        .iter()
        .find(|block| block.original_absolute_path == heap.join("CONTEXT.md"))
        .expect("heap-relative context file should be present");
    assert!(context.read_only);
    let secret = document
        .blocks
        .iter()
        .find(|block| block.original_absolute_path == heap.join("src/secret.txt"))
        .expect("heap secret file should be present");
    assert!(secret.read_only);
    assert!(secret.content_lines.iter().any(|line| line.contains("REDACTED")));
    assert!(
        secret
            .content_lines
            .iter()
            .all(|line| !line.contains("AKIA1234567890123456"))
    );
}

#[test]
fn invalid_slopheap_options_fail_instead_of_disabling_slopignore() {
    let temp = tempdir().expect("tempdir should exist");
    let home = temp.path().join("home");
    let pocket = temp.path().join("pocket");
    let heap = temp.path().join("project");
    fs::create_dir_all(&pocket).expect("pocket should exist");
    fs::create_dir_all(&heap).expect("heap should exist");
    fs::write(pocket.join("private.txt"), "must-not-leak").expect("private file");
    fs::write(
        pocket.join(".slopignore"),
        format!("*\n\\/ {}\n  + src/\n/\\ --include-grph\n", heap.display()),
    )
    .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&pocket)
        .arg(".")
        .assert()
        .failure()
        .stderr(contains("could not parse slopheap options"))
        .stderr(contains("--include-grph"));
}

#[test]
fn outer_and_slopheap_selection_options_compose() {
    let temp = tempdir().expect("tempdir should exist");
    let home = temp.path().join("home");
    let pocket = temp.path().join("pocket");
    let heap = temp.path().join("project");
    let output_dir = temp.path().join("out");
    fs::create_dir_all(&pocket).expect("pocket should exist");
    fs::create_dir_all(&heap).expect("heap should exist");
    fs::write(pocket.join("outer.rs"), "pub fn outer_selected() {}\n").expect("outer seed");
    fs::write(heap.join("base.rs"), "pub fn heap_base() {}\n").expect("heap base");
    fs::write(heap.join("selected.rs"), "pub fn heap_selected() {}\n").expect("heap seed");
    fs::write(heap.join("excluded.rs"), "pub fn heap_excluded() {}\n").expect("excluded seed");
    fs::write(heap.join("ignored.rs"), "pub fn heap_ignored() {}\n").expect("ignored seed");
    fs::write(
        heap.join("outer-excluded.rs"),
        "pub fn outer_excluded() {}\n",
    )
    .expect("outer excluded seed");
    fs::write(
        heap.join("gitignored.rs"),
        "pub fn outer_gitignored() {}\n",
    )
    .expect("outer gitignored seed");
    fs::write(heap.join(".gitignore"), "gitignored.rs\n").expect("heap gitignore");
    fs::write(
        pocket.join(".slopignore"),
        format!(
            "*\n\\/ {}\n  + base.rs\n  ignored.rs\n/\\ --seed selected.rs --seed excluded.rs --seed ignored.rs --seed outer-excluded.rs --seed gitignored.rs -x excluded.rs\n",
            heap.display()
        ),
    )
    .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&pocket)
        .args([
            "--seed",
            "outer.rs",
            "--respect-gitignore",
            "-x",
            "outer-excluded.rs",
            "-o",
        ])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(slop.contains("outer_selected"));
    assert!(slop.contains("heap_selected"));
    assert!(slop.contains("heap_base"));
    assert!(!slop.contains("heap_excluded"));
    assert!(!slop.contains("heap_ignored"));
    assert!(!slop.contains("outer_excluded"));
    assert!(!slop.contains("outer_gitignored"));
}

#[test]
fn repeated_slopheaps_keep_their_own_file_sets() {
    let temp = tempdir().expect("tempdir should exist");
    let pocket = temp.path().join("pocket");
    let heap = temp.path().join("project");
    fs::create_dir_all(&pocket).expect("pocket should exist");
    fs::create_dir_all(&heap).expect("heap should exist");
    let first = heap.join("first.rs");
    let second = heap.join("second.rs");
    fs::write(&first, "pub fn first() {}\n").expect("first heap file");
    fs::write(&second, "pub fn second() {}\n").expect("second heap file");
    fs::write(
        pocket.join(".slopignore"),
        format!(
            "*\n\\/ {}\n  + first.rs\n/\\ -g\n\\/ {}\n  + second.rs\n/\\ --redact\n",
            heap.display(),
            heap.display()
        ),
    )
    .expect("slopignore should be written");

    let report = slop::pathing::collect_source_files_reporting(
        std::slice::from_ref(&pocket),
        Some(usize::MAX),
        &[],
        false,
    )
    .expect("heap files should collect");
    assert_eq!(report.slopheaps.len(), 2);
    assert_eq!(report.slopheaps[0].files, vec![first]);
    assert_eq!(report.slopheaps[1].files, vec![second]);
}

fn read_only_slop(output_dir: &Path) -> String {
    let entry = fs::read_dir(output_dir)
        .expect("output dir should exist")
        .next()
        .expect("a slop file should have been written")
        .expect("dir entry should be readable");
    fs::read_to_string(entry.path()).expect("slop file should be readable")
}

fn star_supersede_fixture() -> tempfile::TempDir {
    let temp = tempdir().expect("tempdir should exist");
    let root = temp.path().join("project_root");
    fs::create_dir_all(root.join("src")).expect("dirs should be created");
    fs::create_dir_all(root.join("docs")).expect("dirs should be created");
    fs::write(root.join("important.txt"), "keep").expect("file should be written");
    fs::write(root.join("junk.log"), "noise").expect("file should be written");
    fs::write(root.join("src/main.rs"), "main").expect("file should be written");
    fs::write(root.join("src/util.rs"), "util").expect("file should be written");
    fs::write(root.join("docs/readme.md"), "readme").expect("file should be written");
    fs::write(root.join("docs/notes.txt"), "notes").expect("file should be written");
    temp
}

#[test]
fn star_supersede_with_slopinclude_slopping() {
    let temp = star_supersede_fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");
    let output_dir = temp.path().join("out");

    fs::write(
        root.join(".slopignore"),
        "*\n+ important.txt\n+ src/\n+ docs/*.md\n",
    )
    .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(&output_dir)
        .arg(".")
        .assert()
        .success();

    let slop = read_only_slop(&output_dir);
    assert!(slop.contains("keep"), "+ important.txt should supersede *");
    assert!(slop.contains("main"), "+ src/ should include nested files");
    assert!(slop.contains("util"), "+ src/ should include nested files");
    assert!(slop.contains("readme"), "+ docs/*.md should supersede *");
    assert!(!slop.contains("noise"), "* should ignore junk.log");
    assert!(!slop.contains("notes"), "* should ignore docs/notes.txt (not .md)");
}

#[test]
fn star_supersede_verbose_shows_ignored() {
    let temp = star_supersede_fixture();
    let home = temp.path().join("home");
    let root = temp.path().join("project_root");

    fs::write(root.join(".slopignore"), "*\n+ important.txt\n+ src/\n")
        .expect("slopignore should be written");

    cargo_bin(&home)
        .current_dir(&root)
        .args(["-r", "-o"])
        .arg(temp.path().join("out"))
        .args(["--verbose", "."])
        .assert()
        .success()
        // Rescued paths are shown as included, not tagged [IGNORED].
        .stderr(contains("important.txt\n"))
        .stderr(contains("important.txt [IGNORED]").not())
        .stderr(contains("src [IGNORED]").not())
        .stderr(contains("main.rs"))
        // Genuinely ignored paths are still reported.
        .stderr(contains("junk.log [IGNORED]"))
        .stderr(contains("docs [IGNORED]"));
}
