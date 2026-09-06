//! `slop <paths> --project-graph`: build a repository's graph and nothing else.
//!
//! This mode does not slopify. The paths on the command line are read only as a
//! way of naming repositories — point at a file, a directory, or a dozen of
//! each, and what comes back is one graph per distinct repository containing
//! them. That is a deliberate departure from every other slop invocation, where
//! the inputs are the payload, and it is why the flag takes over the command
//! rather than decorating it: a run that both bundled files and rebuilt a graph
//! would be two operations sharing one set of arguments and one exit code.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::SlopError;
use crate::graph::find_git_root;
use crate::graphstore::{self, BuildReport, model::ProjectGraph, render, store};
use crate::models::CliArgs;
use crate::pathing::{resolve_absolute, resolve_output_dir};
use crate::tree;

/// Build or refresh the project graph for every repository named by `args`.
///
/// Returns the artifact paths written, in the order the repositories were
/// resolved, so the caller can open the output directory afterwards.
pub fn run_project_graph(args: &CliArgs, config: &Config) -> Result<Vec<PathBuf>, SlopError> {
    let cwd = std::env::current_dir().map_err(|error| SlopError::FileReadFailure {
        path: PathBuf::from("."),
        source: error,
    })?;

    let roots = resolve_repo_roots(&args.inputs, &cwd)?;

    let output_dir = resolve_output_dir(
        args.output_dir
            .as_deref()
            .or(args.slop_to.as_deref())
            .or(config.slopified_folder.as_deref()),
        &cwd,
    )?;

    let mut artifacts = Vec::new();

    for root in &roots {
        if !args.silent {
            eprintln!("Graphing {} ...", root.display());
        }

        // `--reindex` already means "rebuild the derived index you keep for
        // this repo". The graph is another such index, so it answers to the
        // same flag rather than inventing a second one to remember.
        let (graph, report) = graphstore::refresh_project_graph(root, config, args.reindex)?;

        if !args.silent {
            print_summary(&graph, &report, &store::project_graph_path(config, root));
        }

        if config.graph_emit_artifact {
            let artifact = write_artifact(&output_dir, &graph, &report)?;
            if !args.silent {
                let size = fs::metadata(&artifact)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                eprintln!(
                    "  artifact: {} ({})",
                    artifact.display(),
                    tree::format_size(size)
                );
            }
            artifacts.push(artifact);
        }
    }

    Ok(artifacts)
}

/// Map input paths to the repositories that contain them.
///
/// A file resolves through its parent directory; a directory resolves through
/// itself. Duplicates collapse, so `slop src/a.rs src/b.rs --project-graph`
/// builds one graph rather than two identical ones.
fn resolve_repo_roots(inputs: &[PathBuf], cwd: &Path) -> Result<Vec<PathBuf>, SlopError> {
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();
    let mut unresolved: Vec<PathBuf> = Vec::new();

    for input in inputs {
        let absolute = resolve_absolute(input, cwd)?;
        if !absolute.exists() {
            return Err(SlopError::MissingInputPath(absolute));
        }

        let anchor = if absolute.is_dir() {
            absolute.clone()
        } else {
            absolute
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| absolute.clone())
        };

        match find_git_root(&anchor) {
            Some(root) => {
                roots.insert(root);
            }
            None => unresolved.push(absolute),
        }
    }

    if roots.is_empty() {
        let offender = unresolved
            .into_iter()
            .next()
            .unwrap_or_else(|| cwd.to_path_buf());
        return Err(SlopError::GraphRepoRootUnresolved(offender));
    }

    for path in unresolved {
        eprintln!(
            "warning: {} is not inside a git repository; it contributes no graph",
            path.display()
        );
    }

    Ok(roots.into_iter().collect())
}

fn write_artifact(
    output_dir: &Path,
    graph: &ProjectGraph,
    report: &BuildReport,
) -> Result<PathBuf, SlopError> {
    fs::create_dir_all(output_dir).map_err(|error| SlopError::DirectoryCreationFailure {
        path: output_dir.to_path_buf(),
        source: error,
    })?;

    // Named by repo id rather than by directory name: two checkouts called
    // `api` would otherwise overwrite each other's artifact in one folder.
    let path = output_dir.join(format!("{}.project-graph.md", graph.repo_id));
    let body = render::render(graph, Some(report));

    fs::write(&path, body).map_err(|error| SlopError::FileWriteFailure {
        path: path.clone(),
        source: error,
    })?;

    Ok(path)
}

fn print_summary(graph: &ProjectGraph, report: &BuildReport, cache_path: &Path) {
    let stats = &graph.stats;
    eprintln!("  {}", report.summary());
    eprintln!(
        "  {} file(s), {} parsed, {} symbol edge(s), {} co-change edge(s)",
        stats.file_count, stats.parsed_count, stats.symbol_edge_count, stats.cochange_edge_count
    );

    let modules = render::module_overview(graph);
    if modules.is_empty() {
        eprintln!("  no multi-file modules found");
    } else {
        let preview: Vec<String> = modules
            .iter()
            .take(5)
            .map(|(label, count)| format!("{label} ({count})"))
            .collect();
        eprintln!(
            "  {} module(s), modularity {:.3}: {}{}",
            stats.community_count,
            stats.modularity,
            preview.join(", "),
            if modules.len() > 5 { ", ..." } else { "" }
        );
    }

    if report.commits_scanned > 0 {
        eprintln!(
            "  co-change mined from {} commit(s)",
            report.commits_scanned
        );
    }
    eprintln!("  graph: {}", cache_path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_outside_any_repository_is_a_clear_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("loose.rs");
        fs::write(&file, "fn main() {}").expect("write");

        let error = resolve_repo_roots(&[file], dir.path())
            .expect_err("a loose file has no repository to graph");
        assert!(error.to_string().contains("no git repository"), "{error}");
    }

    #[test]
    fn a_missing_input_is_reported_before_anything_is_built() {
        let dir = tempfile::tempdir().expect("tempdir");
        let error = resolve_repo_roots(&[dir.path().join("absent.rs")], dir.path())
            .expect_err("missing input");
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn several_paths_in_one_repository_collapse_to_a_single_root() {
        let cwd = std::env::current_dir().expect("cwd");
        let Some(root) = find_git_root(&cwd) else {
            // Not run from a checkout; nothing to assert about.
            return;
        };
        let roots = resolve_repo_roots(&[cwd.clone(), cwd.clone()], &cwd).expect("roots");
        assert_eq!(roots, vec![root]);
    }
}
