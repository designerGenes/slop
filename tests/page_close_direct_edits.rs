use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use slop::cli::parse_cli_args_from;
use slop::config::Config;
use slop::graphstore::Tier;
use slop::graphstore::page::{PageCloseSource, PageStatus, load_page, open_pages};

static CWD_LOCK: Mutex<()> = Mutex::new(());

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git is available");
    assert!(status.success(), "git {args:?} failed");
}

fn fixture(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "test"]);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "pub fn main() {}\n").unwrap();
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
fn allow_empty_is_rejected_outside_page_close() {
    let error = parse_cli_args_from(["slop", "--allow-empty", "src/main.rs"])
        .expect_err("allow-empty only applies to page close");
    assert!(error.to_string().contains("only be used with --page-close"));
}

#[test]
fn page_close_records_direct_edits_without_a_returned_document() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = config(state.path());
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();

    slop::page::run(&args(&["--page-open", "src/main.rs"]), &config).expect("open");
    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let page = open_pages(&config, &repo_id).pop().expect("one open page");
    let edited = "pub fn main() { println!(\"direct edit\"); }\n";
    fs::write(repo.path().join("src/main.rs"), edited).unwrap();

    slop::page::run(&args(&["--page-close"]), &config).expect("close direct edit");

    assert_eq!(
        fs::read_to_string(repo.path().join("src/main.rs")).unwrap(),
        edited
    );
    let closed = load_page(&slop::graphstore::page::manifest_path(
        &config,
        &repo_id,
        &page.page_id,
    ))
    .unwrap();
    assert_eq!(closed.status, PageStatus::Closed);
    assert_eq!(closed.closed_changes.len(), 1);
    assert_eq!(closed.closed_changes[0].rel, "src/main.rs");
    assert_eq!(closed.closed_changes[0].source, PageCloseSource::Direct);

    std::env::set_current_dir(original_cwd).unwrap();
}

#[test]
fn page_close_refuses_an_empty_page_without_override() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = config(state.path());
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();

    slop::page::run(&args(&["--page-open", "src/main.rs"]), &config).expect("open");
    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let page = open_pages(&config, &repo_id).pop().expect("one open page");
    let error = slop::page::run(&args(&["--page-close"]), &config).expect_err("empty close");
    assert!(
        error
            .to_string()
            .contains("no returned slop documents or direct edits")
    );
    let still_open = load_page(&slop::graphstore::page::manifest_path(
        &config,
        &repo_id,
        &page.page_id,
    ))
    .unwrap();
    assert_eq!(still_open.status, PageStatus::Open);

    std::env::set_current_dir(original_cwd).unwrap();
}

#[test]
fn page_close_allow_empty_closes_an_abandoned_page() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = config(state.path());
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();

    slop::page::run(&args(&["--page-open", "src/main.rs"]), &config).expect("open");
    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let page = open_pages(&config, &repo_id).pop().expect("one open page");
    slop::page::run(&args(&["--page-close", "--allow-empty"]), &config).expect("allow empty close");

    let closed = load_page(&slop::graphstore::page::manifest_path(
        &config,
        &repo_id,
        &page.page_id,
    ))
    .unwrap();
    assert_eq!(closed.status, PageStatus::Closed);
    assert!(closed.closed_changes.is_empty());

    std::env::set_current_dir(original_cwd).unwrap();
}

#[test]
fn page_add_create_makes_a_new_file_available_to_the_local_workflow() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = config(state.path());
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();

    slop::page::run(&args(&["--page-open", "src/main.rs"]), &config).expect("open");
    slop::page::run(
        &args(&["--page-add", "--create", "src/new_rule.rs"]),
        &config,
    )
    .expect("add new file");

    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let page = open_pages(&config, &repo_id).pop().expect("one open page");
    assert!(repo.path().join("src/new_rule.rs").is_file());
    assert!(page.files.iter().any(|file| file.rel == "src/new_rule.rs"));
    let context = fs::read_to_string(slop::graphstore::page::context_path(
        &config,
        &repo_id,
        &page.page_id,
    ))
    .unwrap();
    assert!(context.contains("src/new_rule.rs"));

    fs::write(repo.path().join("src/new_rule.rs"), "pub fn rule() {}\n").unwrap();
    slop::page::run(&args(&["--page-close"]), &config).expect("close direct edit");
    let closed = load_page(&slop::graphstore::page::manifest_path(
        &config,
        &repo_id,
        &page.page_id,
    ))
    .unwrap();
    assert!(closed.closed_changes.iter().any(|change| {
        change.rel == "src/new_rule.rs" && change.source == PageCloseSource::Direct
    }));

    std::env::set_current_dir(original_cwd).unwrap();
}

#[test]
fn page_close_records_both_direct_and_returned_changes() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = config(state.path());
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();

    slop::page::run(&args(&["--page-open", "src/main.rs"]), &config).expect("open");
    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let page = open_pages(&config, &repo_id).pop().expect("one open page");
    fs::write(
        repo.path().join("src/main.rs"),
        "pub fn main() { println!(\"direct\"); }\n",
    )
    .unwrap();
    let returned =
        slop::graphstore::page::page_dir(&config, &repo_id, &page.page_id).join("returned");
    fs::create_dir_all(&returned).unwrap();
    fs::write(
        returned.join("edit.slop.md"),
        format!(
            "#SLOP \"{}\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1\npub fn main() {{ println!(\"returned\"); }}\n",
            repo.path().join("src/main.rs").display()
        ),
    )
    .unwrap();

    slop::page::run(&args(&["--page-close"]), &config).expect("close both sources");
    let closed = load_page(&slop::graphstore::page::manifest_path(
        &config,
        &repo_id,
        &page.page_id,
    ))
    .unwrap();
    assert_eq!(closed.closed_changes.len(), 1);
    assert_eq!(
        closed.closed_changes[0].source,
        PageCloseSource::DirectAndReturned
    );

    std::env::set_current_dir(original_cwd).unwrap();
}

#[test]
fn page_open_uses_the_byte_budget_to_demote_tier_one_to_outlines() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "-q"]);
    git(repo.path(), &["config", "user.email", "test@example.com"]);
    git(repo.path(), &["config", "user.name", "test"]);
    fs::create_dir_all(repo.path().join("src")).unwrap();
    fs::write(
        repo.path().join("src/main.rs"),
        "use crate::a::a;\nuse crate::b::b;\npub fn main() { a(); b(); }\n",
    )
    .unwrap();
    fs::write(
        repo.path().join("src/a.rs"),
        format!("pub fn a() {{}}\n{}", "// filler\n".repeat(300)),
    )
    .unwrap();
    fs::write(
        repo.path().join("src/b.rs"),
        format!("pub fn b() {{}}\n{}", "// filler\n".repeat(300)),
    )
    .unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-q", "-m", "initial"]);

    let state = tempfile::tempdir().unwrap();
    let mut config = config(state.path());
    config.max_slop_bytes = 1_500;
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();

    let tower = slop::graphstore::refresh_tower_graph(
        repo.path(),
        &["src/main.rs".to_string()],
        &config,
        false,
    )
    .expect("tower graph");
    assert!(tower.members.iter().any(|member| member.tier == Tier::One));
    slop::page::run(
        &args(&["--page-open", "--max-slop-bytes", "1500", "src/main.rs"]),
        &config,
    )
    .expect("open within budget");

    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let page = open_pages(&config, &repo_id).pop().expect("one open page");
    assert!(page.files.iter().all(|file| file.tier != Tier::One));
    let context = slop::graphstore::page::context_path(&config, &repo_id, &page.page_id);
    assert!(fs::metadata(&context).unwrap().len() <= 1_500);
    let contents = fs::read_to_string(context).unwrap();
    assert!(contents.contains("TIER 1 (outline only"));

    std::env::set_current_dir(original_cwd).unwrap();
}

#[test]
fn page_open_refuses_a_budget_smaller_than_its_required_context() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = config(state.path());
    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(repo.path()).unwrap();

    let error = slop::page::run(
        &args(&["--page-open", "--max-slop-bytes", "1", "src/main.rs"]),
        &config,
    )
    .expect_err("tier zero and metadata cannot fit in one byte");
    assert!(error.to_string().contains("exceeding the 1-byte budget"));
    let repo_id = slop::graphstore::store::repo_id(repo.path());
    assert!(open_pages(&config, &repo_id).is_empty());

    std::env::set_current_dir(original_cwd).unwrap();
}
