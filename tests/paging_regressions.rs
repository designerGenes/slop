use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use slop::cli::parse_cli_args_from;
use slop::config::Config;
use slop::graphstore::page::open_pages;

static CWD_LOCK: Mutex<()> = Mutex::new(());

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
}

fn fixture(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "test"]);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/a.rs"), "pub fn a() {}\n").unwrap();
    fs::write(root.join("src/b.rs"), "pub fn b() { a(); }\n").unwrap();
    fs::write(root.join("src/c.rs"), "pub fn c() {}\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial"]);
}

fn config(state: &Path) -> Config {
    let mut config = Config::default();
    config.graph_store_dir = Some(state.join("graphs"));
    config.pages_dir = Some(state.join("pages"));
    config.graph_emit_artifact = false;
    config
}

fn args(values: &[&str]) -> slop::models::CliArgs {
    parse_cli_args_from(std::iter::once("slop").chain(values.iter().copied())).unwrap()
}

#[test]
fn page_opens_with_allow_secrets() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    fs::write(
        repo.path().join("src/a.rs"),
        "const KEY: &str = \"AKIAIOSFODNN7EXAMPLE\";\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut config = config(state.path());
    config.secret_scan = "block".to_string();
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();
    let result = slop::page::run(
        &args(&["--page-open", "--allow-secrets", "src/a.rs"]),
        &config,
    );
    std::env::set_current_dir(original).unwrap();
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn identical_page_opens_create_distinct_directories() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = config(state.path());
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();
    let open = args(&["--page-open", "src/a.rs"]);
    slop::page::run(&open, &config).unwrap();
    slop::page::run(&open, &config).unwrap();
    let repo_id = slop::graphstore::store::repo_id(repo.path());
    std::env::set_current_dir(original).unwrap();
    assert_eq!(open_pages(&config, &repo_id).len(), 2);
}

#[test]
fn page_open_rejects_ignored_selection_flags() {
    let error = parse_cli_args_from(["slop", "--page-open", "--match", "a", "src"])
        .expect_err("page-open must reject bundle selection flags");
    assert!(error.to_string().contains("--match"));
}

#[test]
fn tower_respects_gitignore_when_requested() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    fs::write(repo.path().join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir_all(repo.path().join("ignored")).unwrap();
    fs::write(
        repo.path().join("ignored/hidden.rs"),
        "pub fn hidden() {}\n",
    )
    .unwrap();
    git(repo.path(), &["add", "-f", "ignored/hidden.rs"]);
    let state = tempfile::tempdir().unwrap();
    let config = config(state.path());
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();
    let (_, without_flag) =
        slop::tower_graph::resolve_tower_seeds(&args(&["--tower-graph", "-r", "."]), &config)
            .unwrap();
    let (_, with_flag) = slop::tower_graph::resolve_tower_seeds(
        &args(&["--tower-graph", "-r", "--respect-gitignore", "."]),
        &config,
    )
    .unwrap();
    std::env::set_current_dir(original).unwrap();
    assert!(without_flag.iter().any(|path| path == "ignored/hidden.rs"));
    assert!(!with_flag.iter().any(|path| path == "ignored/hidden.rs"));
}

#[test]
fn bad_later_returned_document_does_not_apply_earlier_edits() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = config(state.path());
    let original = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();
    slop::page::run(&args(&["--page-open", "src/a.rs"]), &config).unwrap();
    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let page = open_pages(&config, &repo_id).pop().unwrap();
    let returned =
        slop::graphstore::page::page_dir(&config, &repo_id, &page.page_id).join("returned");
    fs::create_dir_all(&returned).unwrap();
    fs::write(
        returned.join("1.returned.slop.md"),
        format!(
            "#SLOP \"{}\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1\npub fn changed() {{}}\n",
            repo.path().join("src/a.rs").display()
        ),
    )
    .unwrap();
    fs::write(
        returned.join("2.returned.slop.md"),
        format!(
            "#SLOP \"{}\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1\npub fn hijacked() {{}}\n",
            repo.path().join("src/c.rs").display()
        ),
    )
    .unwrap();
    let result = slop::page::run(&args(&["--page-close"]), &config);
    std::env::set_current_dir(original).unwrap();
    assert!(result.is_err());
    assert_eq!(
        fs::read_to_string(repo.path().join("src/a.rs")).unwrap(),
        "pub fn a() {}\n"
    );
}
