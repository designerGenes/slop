//! Behavioral checks for Stage 2/3 as actually wired together: real git
//! repos, real CLI parsing, real page open/add/close. Unit tests already
//! cover the algorithms in isolation; this exercises the seams between them
//! and the specific risks a read-through turned up.
//!
//! Page commands resolve "the repo" from the process cwd, so every test that
//! touches one serializes on CWD_LOCK — cargo runs tests in parallel threads
//! within one process, and a shared cwd is global mutable state.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use slop::cli::parse_cli_args_from;
use slop::config::Config;
use slop::graphstore::page::{PageStatus, load_page, open_pages};
use slop::graphstore::refresh_project_graph;

static CWD_LOCK: Mutex<()> = Mutex::new(());

/// A test that panics while cwd is inside a tempdir leaves the process cwd
/// pointing at a deleted directory, which makes `current_dir()` fail for every
/// test that runs after it. Fall back to the crate root in that case.
fn safe_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        std::env::set_current_dir(&root).expect("crate root is a valid cwd");
        root
    })
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git available");
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "t@example.com"]);
    git(dir, &["config", "user.name", "t"]);
}

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn test_config(dir: &Path) -> Config {
    let mut config = Config::default();
    config.graph_store_dir = Some(dir.join("cache").join("graphs"));
    config.pages_dir = Some(dir.join("pages"));
    config.slopified_folder = Some(dir.join("slopified"));
    config.graph_emit_artifact = false;
    config
}

fn fixture(root: &Path) {
    init_repo(root);
    write(
        root,
        "src/hash.rs",
        "pub fn hash_password(x: &str) -> String {\n    x.to_string()\n}\n",
    );
    write(
        root,
        "src/login.rs",
        "use crate::hash::hash_password;\n\npub fn login(u: &str) {\n    hash_password(u);\n}\n",
    );
    write(root, "src/unrelated.rs", "pub fn unrelated() {}\n");
    commit(root, "initial");
}

fn cli(argv: &[&str]) -> slop::models::CliArgs {
    let mut full = vec!["slop".to_string()];
    full.extend(argv.iter().map(|s| s.to_string()));
    parse_cli_args_from(full).expect("cli parses")
}

/// The load-bearing lifecycle test: open with a real seed, confirm the
/// context file and manifest look like the spec, edit through a returned
/// document, close, and confirm the real file changed and the graph refreshed.
#[test]
fn full_page_lifecycle_applies_edits_and_refreshes_the_graph() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = test_config(state.path());

    let original_cwd = safe_cwd();
    std::env::set_current_dir(repo.path()).unwrap();

    let open_args = cli(&["--page-open", "src/login.rs"]);
    slop::page::run(&open_args, &config).expect("open");

    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let pages = open_pages(&config, &repo_id);
    assert_eq!(pages.len(), 1, "exactly one page should be open");
    let page = &pages[0];
    assert_eq!(page.status, PageStatus::Open);
    assert!(page.files.iter().any(|f| f.rel == "src/login.rs"));

    let context_path = slop::graphstore::page::context_path(&config, &repo_id, &page.page_id);
    let context = fs::read_to_string(&context_path).expect("context file exists");
    assert!(context.contains("CONTEXT PAGE"), "{context}");
    assert!(
        context.contains("hash_password") || context.contains("src/hash.rs"),
        "{context}"
    );

    // Simulate the agent's returned edit: login() now also calls something new.
    let edited = "use crate::hash::hash_password;\n\npub fn login(u: &str) {\n    hash_password(u);\n    println!(\"logged in\");\n}\n";
    let original_sha = page
        .files
        .iter()
        .find(|f| f.rel == "src/login.rs")
        .unwrap()
        .base_sha
        .clone();
    let returned_dir =
        slop::graphstore::page::page_dir(&config, &repo_id, &page.page_id).join("returned");
    fs::create_dir_all(&returned_dir).unwrap();
    let returned_body = format!(
        "#SLOP \"{}\" #SLOPED_LINES {} #SLOP_TRAILING_NEWLINE 1 #SLOP_BASE_SHA {}\n{}",
        repo.path().join("src/login.rs").display(),
        edited.lines().count(),
        original_sha,
        edited,
    );
    fs::write(returned_dir.join("1.returned.slop.md"), returned_body).unwrap();

    let close_args = cli(&["--page-close"]);
    slop::page::run(&close_args, &config).expect("close");

    let on_disk = fs::read_to_string(repo.path().join("src/login.rs")).unwrap();
    assert_eq!(
        on_disk, edited,
        "the edit should have landed on the real file"
    );

    let closed = load_page(&slop::graphstore::page::manifest_path(
        &config,
        &repo_id,
        &page.page_id,
    ))
    .unwrap();
    assert_eq!(closed.status, PageStatus::Closed, "page should be closed");

    let (project, _) =
        refresh_project_graph(repo.path(), &config, false).expect("graph reads back");
    assert!(project.file("src/login.rs").is_some());

    std::env::set_current_dir(original_cwd).unwrap();
}

/// The security-relevant case: a returned document naming a file the page
/// never opened or added must be rejected, and rejected *before* any write
/// happens — a partial apply that writes 2 of 3 files then errors on the
/// third would be worse than an outright refusal.
#[test]
fn a_returned_write_outside_page_scope_is_rejected_and_nothing_is_written() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = test_config(state.path());

    let original_cwd = safe_cwd();
    std::env::set_current_dir(repo.path()).unwrap();

    let open_args = cli(&["--page-open", "src/login.rs"]);
    slop::page::run(&open_args, &config).expect("open");

    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let page = &open_pages(&config, &repo_id)[0];
    let unrelated_before = fs::read_to_string(repo.path().join("src/unrelated.rs")).unwrap();

    let returned_dir =
        slop::graphstore::page::page_dir(&config, &repo_id, &page.page_id).join("returned");
    fs::create_dir_all(&returned_dir).unwrap();
    // src/unrelated.rs was never part of this page.
    let sneaky = format!(
        "#SLOP \"{}\" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1\npub fn hijacked() {{}}\n",
        repo.path().join("src/unrelated.rs").display(),
    );
    fs::write(returned_dir.join("1.returned.slop.md"), sneaky).unwrap();

    let close_args = cli(&["--page-close"]);
    let result = slop::page::run(&close_args, &config);
    assert!(result.is_err(), "an out-of-scope write must be rejected");
    assert!(
        result.unwrap_err().to_string().contains("outside"),
        "error should say the write is out of scope"
    );

    let unrelated_after = fs::read_to_string(repo.path().join("src/unrelated.rs")).unwrap();
    assert_eq!(
        unrelated_before, unrelated_after,
        "the unrelated file must be untouched"
    );

    let still_open = load_page(&slop::graphstore::page::manifest_path(
        &config,
        &repo_id,
        &page.page_id,
    ))
    .unwrap();
    assert_eq!(
        still_open.status,
        PageStatus::Open,
        "a rejected close must not mark the page closed"
    );

    std::env::set_current_dir(original_cwd).unwrap();
}

/// CHARACTERIZATION: `--respect-gitignore` is now correctly threaded into
/// `resolve_tower_seeds` (it was hardcoded to `false` before), but it cannot
/// change the outcome, and this test pins down why.
///
/// `resolve_tower_seeds` intersects its walk with the project graph
/// (`if project.file(&rel).is_some()`), and the project graph's file list comes
/// from `repomap::manifest::collect_repo_files`, which prefers `git ls-files`.
/// Git never lists ignored files, so a gitignored file cannot be a tower seed
/// whether or not the flag is passed. The flag is inert here by construction.
///
/// This matters because the project treats a silently-ignored flag as a defect
/// (see `validate_tower_graph_options`, which rejects every other inapplicable
/// flag rather than dropping it). `--respect-gitignore` is now the one
/// inapplicable flag on `--tower-graph` that is still accepted and does nothing.
/// Either reject it there too, or document it as a no-op.
#[test]
fn respect_gitignore_is_inert_on_tower_graph_because_seeds_come_from_git_ls_files() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    write(repo.path(), ".gitignore", "ignored/\n");
    write(repo.path(), "src/main.rs", "pub fn main() {}\n");
    write(
        repo.path(),
        "ignored/secret.rs",
        "pub fn should_not_appear() {}\n",
    );
    commit(repo.path(), "initial");

    let state = tempfile::tempdir().unwrap();
    let config = test_config(state.path());
    let original_cwd = safe_cwd();
    std::env::set_current_dir(repo.path()).unwrap();

    // -r so the walk itself genuinely descends into ignored/; the filtering
    // under test happens after the walk, at the project-graph intersection.
    let without_flag = cli(&["--tower-graph", "-r", "."]);
    let (_, seeds_without) =
        slop::tower_graph::resolve_tower_seeds(&without_flag, &config).expect("seeds resolve");

    let with_flag = cli(&["--tower-graph", "-r", "--respect-gitignore", "."]);
    let (_, seeds_with) =
        slop::tower_graph::resolve_tower_seeds(&with_flag, &config).expect("seeds resolve");
    std::env::set_current_dir(&original_cwd).unwrap();

    assert!(
        !seeds_without
            .iter()
            .any(|rel| rel.contains("ignored/secret.rs")),
        "a gitignored file reached the seed set WITHOUT the flag; that would mean the project \
         graph is no longer git-tracked-only and this test's premise needs revisiting. got {seeds_without:?}"
    );
    assert_eq!(
        seeds_without, seeds_with,
        "--respect-gitignore changed the tower seed set; if that is now intended, this \
         characterization test should be replaced with a real behavioral assertion"
    );
}

/// SPEC GAP: `--page-open` has no size budget of any kind.
///
/// Tier membership is cut by cumulative RWR *mass fraction*
/// (`tower_tier1_mass_fraction`, default 0.60), which says nothing about how
/// many files that is or how large they are. `validate_page_open_options`
/// actively rejects `--max-slop-bytes`, and `page_tier1_include` is a boolean,
/// so the only two reachable page sizes are "seed files only" and "all of
/// tier 1" — with nothing in between.
///
/// Measured on django (single-file seed, 2138-file graph): tier 1 is 271 files
/// and the context page is 10.3 MB. See the review report for the full curve.
///
/// DELETE THIS TEST when a page budget lands; it asserts the absence of one.
#[test]
fn page_open_has_no_size_budget_and_tier_one_is_all_or_nothing() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    // A hub plus several neighbours that all reference it, so tier 1 is
    // populated and every neighbour is individually large.
    write(repo.path(), "src/hub.rs", "pub fn hub_symbol() {}\n");
    for index in 0..6 {
        let filler = "// padding to make this file substantial\n".repeat(400);
        write(
            repo.path(),
            &format!("src/leaf{index}.rs"),
            &format!("{filler}pub fn leaf{index}() {{\n    hub_symbol();\n}}\n"),
        );
    }
    commit(repo.path(), "initial");

    let state = tempfile::tempdir().unwrap();
    let mut config = test_config(state.path());
    let original_cwd = safe_cwd();
    std::env::set_current_dir(repo.path()).unwrap();

    // There is no knob that says "give me at most N bytes"; the only control is
    // this boolean, so measure both of its settings and show the cliff.
    config.page_tier1_include = true;
    slop::page::run(&cli(&["--page-open", "--silent", "src/hub.rs"]), &config)
        .expect("page opens with tier 1");
    let with_tier1 = largest_context_page(state.path());

    config.page_tier1_include = false;
    slop::page::run(&cli(&["--page-open", "--silent", "src/hub.rs"]), &config)
        .expect("page opens without tier 1");
    let without_tier1 = smallest_context_page(state.path());
    std::env::set_current_dir(&original_cwd).unwrap();

    assert!(
        with_tier1 > without_tier1,
        "expected tier-1 inclusion to grow the page ({with_tier1} vs {without_tier1})"
    );
    // The gap between the two settings is the range a budget would need to
    // cover and currently cannot reach at all.
    assert!(
        with_tier1 - without_tier1 > 20_000,
        "expected an all-or-nothing cliff between the two page sizes, got {with_tier1} vs \
         {without_tier1}; if this shrank, a budget may have been added — delete this test"
    );
}

fn context_page_sizes(state: &Path) -> Vec<u64> {
    let mut sizes = Vec::new();
    let pages = state.join("pages");
    if let Ok(repos) = fs::read_dir(&pages) {
        for repo in repos.filter_map(Result::ok) {
            if let Ok(entries) = fs::read_dir(repo.path()) {
                for entry in entries.filter_map(Result::ok) {
                    if let Ok(meta) = fs::metadata(entry.path().join("context.slop.md")) {
                        sizes.push(meta.len());
                    }
                }
            }
        }
    }
    sizes
}

fn largest_context_page(state: &Path) -> u64 {
    context_page_sizes(state)
        .into_iter()
        .max()
        .expect("a page exists")
}

fn smallest_context_page(state: &Path) -> u64 {
    context_page_sizes(state)
        .into_iter()
        .min()
        .expect("a page exists")
}

/// secrets::enforce is called with hardcoded (allow_secrets=false,
/// redact=false) in both page::open and page::add. If a user passes
/// --allow-secrets or --redact to --page-open, this test shows whether that
/// has any effect.
#[test]
fn allow_secrets_flag_on_page_open_is_honored_or_this_documents_that_it_is_not() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    init_repo(repo.path());
    // AWS-style secret key pattern likely to trip a high-confidence secret rule.
    write(
        repo.path(),
        "src/config.rs",
        "pub const KEY: &str = \"AKIAIOSFODNN7EXAMPLE\";\n",
    );
    commit(repo.path(), "initial");

    let state = tempfile::tempdir().unwrap();
    let mut config = test_config(state.path());
    config.secret_scan = "block".to_string();

    let original_cwd = safe_cwd();
    std::env::set_current_dir(repo.path()).unwrap();

    let open_args = cli(&["--page-open", "--allow-secrets", "src/config.rs"]);
    let result = slop::page::run(&open_args, &config);

    std::env::set_current_dir(original_cwd).unwrap();

    // Written to PASS once --allow-secrets is threaded through. If this
    // fails with SecretsDetected, page::open is not honoring the flag.
    assert!(
        result.is_ok(),
        "--allow-secrets was passed to --page-open but the open still failed: {:?}",
        result.err()
    );
}

/// Two `--page-open` calls with the same seeds inside the same wall-clock
/// second: page_id is `{seconds}*1000` concatenated with a hash of
/// (root, seeds), so if both inputs are identical, the ids collide and the
/// second open silently overwrites the first page's directory.
#[test]
fn opening_two_pages_with_the_same_seeds_in_the_same_second_does_not_collide() {
    let _guard = CWD_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let repo = tempfile::tempdir().unwrap();
    fixture(repo.path());
    let state = tempfile::tempdir().unwrap();
    let config = test_config(state.path());

    let original_cwd = safe_cwd();
    std::env::set_current_dir(repo.path()).unwrap();

    let args = cli(&["--page-open", "src/login.rs"]);
    slop::page::run(&args, &config).expect("first open");
    let repo_id = slop::graphstore::store::repo_id(repo.path());
    let after_first: Vec<String> = open_pages(&config, &repo_id)
        .into_iter()
        .map(|p| p.page_id)
        .collect();

    // Immediately re-open with identical seeds. On real hardware this lands
    // in the same second essentially every time.
    slop::page::run(&args, &config).expect("second open");
    let after_second: Vec<PathBuf> =
        fs::read_dir(slop::graphstore::page::pages_dir(&config).join(&repo_id))
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();

    std::env::set_current_dir(original_cwd).unwrap();

    assert_eq!(
        after_second.len(),
        after_first.len() + 1,
        "opening twice with identical seeds should produce two distinct page directories, \
         found {} directories after the second open (page_id collision if this is <= {})",
        after_second.len(),
        after_first.len()
    );
}
