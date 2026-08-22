use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::SlopError;
use crate::graph;
use crate::models::{CliArgs, IgnoreReason, IgnoredEntry, SoupMetaBlock, SourceFile};
use crate::pathing::{
    build_output_filename, collect_source_files_reporting_with_slopignore,
    filter_slopheap_selection, filename_token, resolve_absolute, resolve_output_dir,
    should_respect_gitignore,
};
use crate::secrets;
use crate::selection;
use crate::slop_format::{analyze_contents, serialize_document};
use crate::tree;

pub fn run_slop(args: &CliArgs, config: &Config) -> Result<PathBuf, SlopError> {
    let config = config_with_cli_overrides(config, args);
    let config = &config;
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

    let max_depth = if args.recursive {
        Some(usize::MAX)
    } else {
        Some(0)
    };
    let respect_gitignore = should_respect_gitignore(args.respect_gitignore, config);
    let skip_slopignore = args.ignore_slopignore
        || (config.skip_slopignore_for_full_statement
            && !selection::selection_mode(args)
            && resolved_inputs.iter().all(|input| input.is_file()));
    let walk = collect_source_files_reporting_with_slopignore(
        &resolved_inputs,
        max_depth,
        &args.exclude,
        respect_gitignore,
        skip_slopignore,
    )?;
    let mut candidate_files = walk.files;
    let forced_files = walk.forced_files;
    let ignored_entries = walk.ignored;
    let mut slopheaps = walk.slopheaps;

    let mut heap_selection_meta = Vec::new();
    let mut heap_selected_files = Vec::new();
    for heap in &mut slopheaps {
        if !selection::selection_mode(&heap.args) {
            continue;
        }
        let heap_config = config_with_cli_overrides(config, &heap.args);
        let selectors = selection::build_selectors(&heap.args, &heap_config)?;
        let map_reserve = selection::budget::estimate_map_reserve(&heap_config);
        let mut selected = selection::select_files(
            &selectors,
            &heap.root,
            map_reserve,
            &heap_config,
            heap.args.reindex,
        )?;
        let selected_paths = filter_slopheap_selection(
            heap,
            selected
                .selected
                .iter()
                .map(|selected| selected.path.clone())
                .collect(),
            &args.exclude,
            respect_gitignore,
        )?;
        selected
            .selected
            .retain(|selected| selected_paths.contains(&selected.path));
        for dropped in &selected.dropped {
            eprintln!(
                "warning: {} matched in slopheap {} but was cut to stay under budget",
                dropped.rel_path,
                heap.root.display()
            );
        }
        for selected in &selected.selected {
            candidate_files.push(selected.path.clone());
            heap_selected_files.push(selected.path.clone());
            if !heap.files.contains(&selected.path) {
                heap.files.push(selected.path.clone());
            }
        }
        if heap.args.explain_selection || heap_config.selection_provenance {
            heap_selection_meta.push(selection::build_provenance_block(
                &selected,
                &selectors,
                heap_config.selection_provenance_max_bytes,
            ));
        }
    }

    if candidate_files.is_empty() {
        return Err(SlopError::InputExpandedToZeroFiles);
    }

    let corpus_root =
        graph::shared_git_root(&candidate_files).unwrap_or_else(|| resolved_inputs[0].clone());

    let (mut files, selection_meta) = if selection::selection_mode(args) {
        let selectors = selection::build_selectors(args, config)?;
        let map_reserve = selection::budget::estimate_map_reserve(config);
        let sel =
            selection::select_files(&selectors, &corpus_root, map_reserve, config, args.reindex)?;

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

    // Selection may replace the ordinary walk result, but .slopignore include
    // directives are unconditional and must remain in every generated slop.
    let mut seen = BTreeSet::new();
    files.retain(|path| seen.insert(path.clone()));
    for path in heap_selected_files {
        if seen.insert(path.clone()) {
            files.push(path);
        }
    }
    for path in forced_files {
        if seen.insert(path.clone()) {
            files.push(path);
        }
    }

    let verbose = args.verbose || config.verbose_output;
    if !args.silent {
        print_slop_tree(&resolved_inputs, &files, &ignored_entries, verbose);
    }

    let mut source_files = files
        .iter()
        .map(build_source_file)
        .collect::<Result<Vec<_>, _>>()?;

    for ctx_path in &args.context_files {
        let resolved = resolve_absolute(ctx_path, &cwd)?;
        if !resolved.exists() {
            eprintln!(
                "warning: context file {} not found, skipping",
                resolved.display()
            );
            continue;
        }
        if let Some(source) = source_files
            .iter_mut()
            .find(|source| source.original_absolute_path == resolved)
        {
            source.read_only = true;
            source.base_sha = None;
        } else {
            let mut source = build_source_file(&resolved)?;
            source.read_only = true;
            source.base_sha = None;
            source_files.push(source);
        }
    }

    for heap in &slopheaps {
        for ctx_path in &heap.args.context_files {
            let resolved = resolve_absolute(ctx_path, &heap.root)?;
            if !resolved.exists() {
                eprintln!(
                    "warning: slopheap context file {} not found, skipping",
                    resolved.display()
                );
                continue;
            }
            if let Some(source) = source_files
                .iter_mut()
                .find(|source| source.original_absolute_path == resolved)
            {
                source.read_only = true;
                source.base_sha = None;
            } else {
                let mut source = build_source_file(&resolved)?;
                source.read_only = true;
                source.base_sha = None;
                source_files.push(source);
            }
        }
    }

    let source_files = enforce_secrets_by_scope(source_files, &slopheaps, args, config)?;

    let mut meta_blocks = if graph::should_include_graph(args.include_graph, config) {
        build_graph_meta_blocks(&corpus_root, &files, config)?
    } else {
        Vec::new()
    };

    for heap in &slopheaps {
        if !heap.args.include_graph {
            continue;
        }
        let heap_config = config_with_cli_overrides(config, &heap.args);
        let seed_files = heap.files.clone();
        meta_blocks.extend(build_graph_meta_blocks(
            &heap.root,
            &seed_files,
            &heap_config,
        )?);
    }

    meta_blocks.extend(selection_meta);
    meta_blocks.extend(heap_selection_meta);

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

fn config_with_cli_overrides(config: &Config, args: &CliArgs) -> Config {
    let mut effective = config.clone();
    if let Some(graph_map_tokens) = args.graph_map_tokens {
        effective.graph_map_tokens = graph_map_tokens;
    }
    if let Some(graph_format) = &args.graph_format {
        effective.graph_format = graph_format.clone();
    }
    if let Some(top_k) = args.top_k {
        effective.top_k = top_k;
    }
    if let Some(max_slop_bytes) = args.max_slop_bytes {
        effective.max_slop_bytes = max_slop_bytes;
    }
    effective
}

fn enforce_secrets_by_scope(
    source_files: Vec<SourceFile>,
    slopheaps: &[crate::models::SlopHeapRequest],
    args: &CliArgs,
    config: &Config,
) -> Result<Vec<SourceFile>, SlopError> {
    let mut enforced = Vec::with_capacity(source_files.len());
    for source in source_files {
        let mut matching_heaps = slopheaps.iter().filter(|heap| {
            heap.files.contains(&source.original_absolute_path)
                || heap.args.context_files.iter().any(|context| {
                    resolve_absolute(context, &heap.root)
                        .is_ok_and(|path| path == source.original_absolute_path)
                })
        });
        let allow_secrets = args.allow_secrets
            || matching_heaps.clone().any(|heap| heap.args.allow_secrets);
        let redact = args.redact || matching_heaps.any(|heap| heap.args.redact);
        let mut result = secrets::enforce(
            std::slice::from_ref(&source),
            config,
            allow_secrets,
            redact,
        )?;
        enforced.append(&mut result);
    }
    Ok(enforced)
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
fn print_slop_tree(inputs: &[PathBuf], files: &[PathBuf], ignored: &[IgnoredEntry], verbose: bool) {
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
    corpus_root: &Path,
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
