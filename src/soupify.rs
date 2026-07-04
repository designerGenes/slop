use std::fs;
use std::path::PathBuf;

use crate::config::Config;
use crate::error::SoupifyError;
use crate::graph;
use crate::models::{CliArgs, SoupMetaBlock, SourceFile};
use crate::pathing::{
    build_output_filename, collect_source_files, filename_token, resolve_absolute,
    resolve_output_dir, should_respect_gitignore,
};
use crate::secrets;
use crate::selection;
use crate::soup_format::{analyze_contents, serialize_document};

pub fn run_soupify(args: &CliArgs, config: &Config) -> Result<PathBuf, SoupifyError> {
    let cwd = std::env::current_dir().map_err(|error| SoupifyError::FileReadFailure {
        path: PathBuf::from("."),
        source: error,
    })?;

    let output_dir = resolve_output_dir(
        args.output_dir
            .as_deref()
            .or(args.soupify_to.as_deref())
            .or(config.soupified_folder.as_deref()),
        &cwd,
    )?;
    let resolved_inputs = args
        .inputs
        .iter()
        .map(|input| resolve_absolute(input, &cwd))
        .collect::<Result<Vec<_>, _>>()?;

    for input in &resolved_inputs {
        if !input.exists() {
            return Err(SoupifyError::MissingInputPath(input.clone()));
        }
    }

    let max_depth = if args.recursive { Some(usize::MAX) } else { Some(0) };
    let respect_gitignore = should_respect_gitignore(args.respect_gitignore, config);
    let candidate_files =
        collect_source_files(&resolved_inputs, max_depth, &args.exclude, respect_gitignore)?;
    if candidate_files.is_empty() {
        return Err(SoupifyError::InputExpandedToZeroFiles);
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

    fs::create_dir_all(&output_dir).map_err(|error| SoupifyError::DirectoryCreationFailure {
        path: output_dir.clone(),
        source: error,
    })?;

    let output_file = output_dir.join(build_output_filename(&files, !meta_blocks.is_empty())?);
    fs::write(&output_file, markdown).map_err(|error| SoupifyError::FileWriteFailure {
        path: output_file.clone(),
        source: error,
    })?;

    Ok(output_file)
}

fn build_graph_meta_blocks(
    corpus_root: &PathBuf,
    seed_files: &[PathBuf],
    config: &Config,
) -> Result<Vec<SoupMetaBlock>, SoupifyError> {
    let meta_block = graph::generate_repomap(corpus_root, seed_files, config)?;
    Ok(vec![meta_block])
}

fn build_source_file(path: &PathBuf) -> Result<SourceFile, SoupifyError> {
    let bytes = fs::read(path).map_err(|error| SoupifyError::FileReadFailure {
        path: path.clone(),
        source: error,
    })?;
    let contents =
        String::from_utf8(bytes.clone()).map_err(|_| SoupifyError::Utf8DecodeFailure(path.clone()))?;
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
