use std::fs;
use std::path::PathBuf;

use crate::config::Config;
use crate::error::SlopError;
use crate::graph;
use crate::models::{CliArgs, IgnoreReason, IgnoredEntry, SoupMetaBlock, SourceFile};
use crate::pathing::{
    build_output_filename, collect_source_files_reporting, filename_token, resolve_absolute,
    resolve_output_dir, should_respect_gitignore,
};
use crate::secrets;
use crate::selection;
use crate::slop_format::{analyze_contents, serialize_document};
use crate::tree;

pub fn run_slop(args: &CliArgs, config: &Config) -> Result<PathBuf, SlopError> {
    let cwd = std::env::current_dir().map_err(|error| SlopError::FileReadFailure {
        path: PathBuf::from("."),
        source: error,
    })?;

    let output_dir = resolve_output_dir(
        args.output_dir
            .as_deref()
            .or(args.slop_to.as_deref())
            .or(config.slopified_folder.as_deref()),
        &cwd,
    )?;
    let resolved_inputs = args
        .inputs
        .iter()
        .map(|input| resolve_absolute(input, &cwd))
        .collect::<Result<Vec<_>, _>>()?;

    for input in &resolved_inputs {
        if !input.exists() {
            return Err(SlopError::MissingInputPath(input.clone()));
        }
    }

    let max_depth = if args.recursive { Some(usize::MAX) } else { Some(0) };
    let respect_gitignore = should_respect_gitignore(args.respect_gitignore, config);
    let walk = collect_source_files_reporting(
        &resolved_inputs,
        max_depth,
        &args.exclude,
        respect_gitignore,
    )?;
    let candidate_files = walk.files;
    let ignored_entries = walk.ignored;
    if candidate_files.is_empty() {
        return Err(SlopError::InputExpandedToZeroFiles);
    }

    let corpus_root = graph::shared_git_root(&candidate_files)
        .unwrap_or_else(|| resolved_inputs[0].clone());

    let (files, selection_meta) = if selection::selection_mode(args) {
        let selectors = selection::build_selectors(args, config)?;
        let map_reserve = selection::budget::estimate_map_reserve(config);
        let sel = selection::select_files(
            &selectors,
            &corpus_root,
            map_reserve,
            config,
            args.reindex,
        )?;

        for dropped in &sel.dropped {
            eprintln!(
                "warning: {} matched but cut to stay under budget",
                dropped.rel_path
            );
        }

        let meta = if args.explain_selection || config.selection_provenance {
            vec![selection::build_provenance_block(
                &sel,
                &selectors,
                config.selection_provenance_max_bytes,
            )]
        } else {
            Vec::new()
        };

        let paths: Vec<PathBuf> = sel.selected.iter().map(|s| s.path.clone()).collect();
        if paths.is_empty() {
            (candidate_files, meta)
        } else {
            (paths, meta)
        }
    } else {
        (candidate_files, Vec::new())
    };

    let verbose = args.verbose || config.verbose_output;
    if !args.silent {
        print_slop_tree(&resolved_inputs, &files, &ignored_entries, verbose);
    }

    let source_files = files
        .iter()
        .map(build_source_file)
        .collect::<Result<Vec<_>, _>>()?;

    let mut source_files = secrets::enforce(&source_files, config, args.allow_secrets, args.redact)?;

    for ctx_path in &args.context_files {
        let resolved = resolve_absolute(ctx_path, &cwd)?;
        if !resolved.exists() {
            eprintln!("warning: context file {} not found, skipping", resolved.display());
            continue;
        }
        let mut sf = build_source_file(&resolved)?;
        sf.read_only = true;
        sf.base_sha = None;
        source_files.push(sf);
    }

    let mut meta_blocks = if graph::should_include_graph(args.include_graph, config) {
        build_graph_meta_blocks(&corpus_root, &files, config)?
    } else {
        Vec::new()
    };

    meta_blocks.extend(selection_meta);

    let markdown = serialize_document(&meta_blocks, &source_files)?;

    fs::create_dir_all(&output_dir).map_err(|error| SlopError::DirectoryCreationFailure {
        path: output_dir.clone(),
        source: error,
    })?;

    let output_file = output_dir.join(build_output_filename(&files, !meta_blocks.is_empty())?);
    let written_bytes = markdown.len() as u64;
    fs::write(&output_file, markdown).map_err(|error| SlopError::FileWriteFailure {
        path: output_file.clone(),
        source: error,
    })?;

    if !args.silent {
        let size = fs::metadata(&output_file)
            .map(|metadata| metadata.len())
            .unwrap_or(written_bytes);
        eprintln!();
        eprintln!(
            "Slop file written is {} and was written to:",
            tree::format_size(size)
        );
        eprintln!("{}", output_file.display());
    }

    Ok(output_file)
}

/// Anchor the tree at the directory the user actually pointed slop at, so
/// `slop -r .` shows the project root even when every match happens to sit in
/// one deep subdirectory. Anything else (several inputs, a single file) falls
/// back to the deepest shared directory.
fn tree_root(inputs: &[PathBuf], all: &[PathBuf]) -> PathBuf {
    if inputs.len() == 1 && inputs[0].is_dir() {
        return inputs[0].clone();
    }
    tree::common_root(all)
}

/// Print the run report: every file going into the slop, as a tree. With
/// `verbose`, `.slopignore` casualties are shown in place and tagged.
///
/// This goes to stderr, alongside the logo and the warnings, so that stdout
/// stays free for machine consumption.
fn print_slop_tree(
    inputs: &[PathBuf],
    files: &[PathBuf],
    ignored: &[IgnoredEntry],
    verbose: bool,
) {
    let mut all: Vec<PathBuf> = files.to_vec();
    if verbose {
        all.extend(ignored.iter().map(|entry| entry.path.clone()));
    }

    let root = tree_root(inputs, &all);
    eprintln!("Slopifying...");
    eprint!(
        "{}",
        tree::render_walk_tree(&root, files, ignored, verbose, tree::colors_enabled())
    );

    // `-x/--exclude` needs no explanation — the user typed it. A .slopignore
    // sitting in the repo is the surprising one, so nudge about that only.
    let slopignored = ignored
        .iter()
        .filter(|entry| entry.reason == IgnoreReason::SlopIgnore)
        .count();
    if !verbose && slopignored > 0 {
        eprintln!(
            "({slopignored} path(s) skipped by .slopignore; re-run with --verbose to list them)"
        );
    }
}

fn build_graph_meta_blocks(
    corpus_root: &PathBuf,
    seed_files: &[PathBuf],
    config: &Config,
) -> Result<Vec<SoupMetaBlock>, SlopError> {
    let meta_block = graph::generate_repomap(corpus_root, seed_files, config)?;
    Ok(vec![meta_block])
}

fn build_source_file(path: &PathBuf) -> Result<SourceFile, SlopError> {
    let bytes = fs::read(path).map_err(|error| SlopError::FileReadFailure {
        path: path.clone(),
        source: error,
    })?;
    let contents =
        String::from_utf8(bytes.clone()).map_err(|_| SlopError::Utf8DecodeFailure(path.clone()))?;
    let (logical_line_count, trailing_newline) = analyze_contents(&contents);

    let base_sha = blake3::hash(&bytes).to_hex().to_string();

    Ok(SourceFile {
        original_absolute_path: path.clone(),
        file_name: path
            .file_name()
            .expect("collected file should have basename")
            .to_string_lossy()
            .to_string(),
        name_token: filename_token(path)?,
        contents,
        logical_line_count,
        trailing_newline,
        base_sha: Some(base_sha),
        read_only: false,
    })
}
