//! End-to-end coverage for the persisted project graph.
//!
//! The claim stage one makes is narrow and testable: a refreshed graph is
//! identical to one built from scratch, and it gets there without re-parsing
//! files whose bytes did not change. Everything else in the graph store is in
//! service of that claim, so that is what these tests hold it to.

use std::fs;
use std::path::Path;

use slop::config::Config;
use slop::graphstore::{BuildOptions, build_project_graph, render};

fn write(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(path, body).expect("write");
}

/// Two clusters that call within themselves and never across: `auth` and
/// `billing` are separate modules by construction, and any working community
/// detection has to say so.
fn fixture(root: &Path) {
    write(
        root,
        "auth/hash.rs",
        "pub fn hash_password(input: &str) -> String {\n    format!(\"h:{input}\")\n}\n",
    );
    write(
        root,
        "auth/session.rs",
        "use crate::auth::hash::hash_password;\n\npub fn session_new(user: &str) -> String {\n    hash_password(user)\n}\n",
    );
    write(
        root,
        "auth/login.rs",
        "use crate::auth::hash::hash_password;\nuse crate::auth::session::session_new;\n\npub fn login(user: &str) -> String {\n    let _ = hash_password(user);\n    session_new(user)\n}\n",
    );
    write(
        root,
        "billing/tax.rs",
        "pub fn tax_for(amount: u64) -> u64 {\n    amount / 10\n}\n",
    );
    write(
        root,
        "billing/invoice.rs",
        "use crate::billing::tax::tax_for;\n\npub fn invoice_new(amount: u64) -> u64 {\n    amount + tax_for(amount)\n}\n",
    );
    write(
        root,
        "billing/charge.rs",
        "use crate::billing::invoice::invoice_new;\nuse crate::billing::tax::tax_for;\n\npub fn charge(amount: u64) -> u64 {\n    invoice_new(amount) + tax_for(amount)\n}\n",
    );
    write(root, "README.md", "# fixture\n\nNot code.\n");
}

fn options() -> BuildOptions {
    // The fixture has no git history, so co-change contributes nothing here and
    // the clustering is judged on symbol edges alone.
    BuildOptions::from_config(&Config::default(), false)
}

#[test]
fn a_cold_build_sees_every_file_and_parses_only_the_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());

    let (graph, report) = build_project_graph(dir.path(), None, &options()).expect("build");

    assert!(report.cold);
    assert_eq!(graph.stats.file_count, 7, "the README is inventory too");
    assert_eq!(graph.stats.parsed_count, 6, "but it is not parseable");
    assert_eq!(report.reparsed, 6, "the README is never handed to a parser");
    assert!(graph.file("README.md").is_some());
    assert!(!graph.file("README.md").expect("readme").parsed);
}

#[test]
fn the_module_seam_falls_where_the_calls_do() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());
    let (graph, _) = build_project_graph(dir.path(), None, &options()).expect("build");

    let login = graph.file("auth/login.rs").expect("login");
    let session = graph.file("auth/session.rs").expect("session");
    let charge = graph.file("billing/charge.rs").expect("charge");

    assert_eq!(login.community, session.community, "auth belongs together");
    assert_ne!(
        login.community, charge.community,
        "billing is its own module"
    );
}

#[test]
fn depended_upon_files_outrank_the_leaves_that_call_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());
    let (graph, _) = build_project_graph(dir.path(), None, &options()).expect("build");

    let hash = graph.file("auth/hash.rs").expect("hash");
    let login = graph.file("auth/login.rs").expect("login");

    assert!(hash.afferent >= 2, "login and session both call into it");
    assert_eq!(login.afferent, 0, "nothing calls login");
    assert!(
        hash.rank > login.rank,
        "hash {} should outrank login {}",
        hash.rank,
        login.rank
    );
}

#[test]
fn an_unchanged_repository_re_parses_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());

    let (first, _) = build_project_graph(dir.path(), None, &options()).expect("cold");
    let (_, report) = build_project_graph(dir.path(), Some(&first), &options()).expect("warm");

    assert!(!report.cold);
    assert_eq!(report.reparsed, 0);
    assert_eq!(report.reused, 6);
    assert_eq!(report.removed, 0);
}

#[test]
fn editing_one_file_re_parses_exactly_that_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());
    let (first, _) = build_project_graph(dir.path(), None, &options()).expect("cold");

    // charge() stops calling tax_for and starts calling hash_password, which
    // drags a real edge across the module seam.
    write(
        dir.path(),
        "billing/charge.rs",
        "use crate::auth::hash::hash_password;\nuse crate::billing::invoice::invoice_new;\n\npub fn charge(amount: u64) -> u64 {\n    let _ = hash_password(\"x\");\n    invoice_new(amount)\n}\n",
    );

    let (second, report) = build_project_graph(dir.path(), Some(&first), &options()).expect("warm");

    assert_eq!(report.reparsed, 1);
    assert_eq!(report.reused, 5);

    let before = first.file("auth/hash.rs").expect("hash").afferent;
    let after = second.file("auth/hash.rs").expect("hash").afferent;
    assert_eq!(after, before + 1, "the new caller shows up in the metrics");

    assert!(
        second
            .symbol_edges
            .iter()
            .any(|edge| edge.from == "billing/charge.rs" && edge.to == "auth/hash.rs"),
        "the edge the edit created is in the graph"
    );
}

#[test]
fn a_deleted_file_leaves_the_graph_entirely() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());
    let (first, _) = build_project_graph(dir.path(), None, &options()).expect("cold");

    fs::remove_file(dir.path().join("billing/tax.rs")).expect("remove");
    let (second, report) = build_project_graph(dir.path(), Some(&first), &options()).expect("warm");

    assert_eq!(report.removed, 1);
    assert!(second.file("billing/tax.rs").is_none());
    assert!(
        second
            .symbol_edges
            .iter()
            .all(|edge| edge.to != "billing/tax.rs" && edge.from != "billing/tax.rs"),
        "no edge may point at a file that no longer exists"
    );
}

#[test]
fn a_refreshed_graph_is_indistinguishable_from_a_rebuilt_one() {
    // This is the load-bearing test. A cache that can disagree with a rebuild is
    // worse than no cache, because every downstream decision inherits the
    // disagreement silently.
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());
    let (first, _) = build_project_graph(dir.path(), None, &options()).expect("cold");

    write(
        dir.path(),
        "auth/session.rs",
        "use crate::auth::hash::hash_password;\nuse crate::billing::tax::tax_for;\n\npub fn session_new(user: &str) -> String {\n    let _ = tax_for(1);\n    hash_password(user)\n}\n",
    );

    let (incremental, _) =
        build_project_graph(dir.path(), Some(&first), &options()).expect("refreshed");
    let (cold, _) = build_project_graph(dir.path(), None, &options()).expect("rebuilt");

    assert_eq!(
        serde_json::to_value(&incremental.files).expect("files"),
        serde_json::to_value(&cold.files).expect("files"),
    );
    assert_eq!(
        serde_json::to_value(&incremental.symbol_edges).expect("edges"),
        serde_json::to_value(&cold.symbol_edges).expect("edges"),
    );
    assert_eq!(
        serde_json::to_value(&incremental.communities).expect("communities"),
        serde_json::to_value(&cold.communities).expect("communities"),
    );
    assert_eq!(
        serde_json::to_value(&incremental.structure).expect("structure"),
        serde_json::to_value(&cold.structure).expect("structure"),
    );
}

#[test]
fn a_forced_rebuild_ignores_the_cache_it_was_handed() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());
    let (first, _) = build_project_graph(dir.path(), None, &options()).expect("cold");

    let forced = BuildOptions::from_config(&Config::default(), true);
    let (_, report) = build_project_graph(dir.path(), Some(&first), &forced).expect("forced");

    assert!(report.cold, "a forced build reports itself as cold");
    assert_eq!(report.reparsed, 6);
    assert_eq!(report.reused, 0);
}

#[test]
fn the_artifact_describes_the_modules_it_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    fixture(dir.path());
    let (graph, report) = build_project_graph(dir.path(), None, &options()).expect("build");

    let text = render::render(&graph, Some(&report));

    assert!(text.contains("# PROJECT GRAPH"), "{text}");
    assert!(text.contains("## MODULES"), "{text}");
    assert!(text.contains("## METRICS"), "{text}");
    assert!(text.contains("#SLOP_REQUEST"), "{text}");
    assert!(text.contains("auth"), "{text}");
    assert!(text.contains("billing"), "{text}");
}

#[test]
fn an_empty_directory_produces_an_empty_graph_rather_than_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (graph, report) = build_project_graph(dir.path(), None, &options()).expect("build");

    assert_eq!(graph.stats.file_count, 0);
    assert_eq!(report.reparsed, 0);
    assert!(graph.communities.is_empty());
    assert!(render::render(&graph, Some(&report)).contains("# PROJECT GRAPH"));
}
