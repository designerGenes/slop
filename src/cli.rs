use std::path::{Path, PathBuf};

use clap::Parser;
use clap::error::ErrorKind;

use crate::error::SlopError;
use crate::models::CliArgs;

#[derive(Debug, Parser)]
#[command(
    name = "slop",
    version,
    about = "Combine files into a markdown slop",
    before_help = crate::logo::LOGO,
)]
struct RawCliArgs {
    #[arg(short = 'o', long = "output")]
    output_dir: Option<PathBuf>,
    #[arg(short = 'd', long = "deslop")]
    deslop: bool,
    #[arg(short = 's', long = "show")]
    show_output_dir: bool,
    #[arg(short = 'r', long = "recursive")]
    recursive: bool,
    #[arg(short = 'x', long = "exclude")]
    exclude: Vec<String>,
    #[arg(long = "respect-gitignore")]
    respect_gitignore: bool,
    #[arg(long = "ignore-slopignore")]
    ignore_slopignore: bool,
    #[arg(short = 'g', long = "include-graph")]
    include_graph: bool,
    /// Exclusive mode: build the project graph of every repository containing
    /// the inputs. Nothing is slopified.
    #[arg(long = "project-graph")]
    project_graph: bool,
    #[arg(long = "tower-graph")]
    tower_graph: bool,
    #[arg(long = "page-open")]
    page_open: bool,
    #[arg(long = "page-add")]
    page_add: bool,
    #[arg(long = "page-close")]
    page_close: bool,
    #[arg(long = "page-list")]
    page_list: bool,
    #[arg(long = "page-prune")]
    page_prune: bool,
    #[arg(long = "page", value_name = "ID")]
    page_id: Option<String>,
    #[arg(long = "older-than", value_name = "DURATION")]
    older_than: Option<String>,
    #[arg(long = "slop-to")]
    slop_to: Option<PathBuf>,
    #[arg(long = "graph-format")]
    graph_format: Option<String>,
    #[arg(long = "graph-map-tokens")]
    graph_map_tokens: Option<usize>,
    #[arg(long = "match", value_name = "TERM")]
    match_terms: Vec<String>,
    #[arg(long = "seed", value_name = "FILE")]
    seeds: Vec<PathBuf>,
    #[arg(long = "hops", value_name = "N")]
    hops: Option<usize>,
    #[arg(long = "symbol", value_name = "NAME")]
    symbols: Vec<String>,
    #[arg(long = "task", value_name = "PROSE")]
    task: Option<String>,
    #[arg(long = "top-k", value_name = "N")]
    top_k: Option<usize>,
    #[arg(long = "max-slop-bytes", value_name = "N")]
    max_slop_bytes: Option<usize>,
    #[arg(long = "reindex")]
    reindex: bool,
    #[arg(long = "explain-selection")]
    explain_selection: bool,
    #[arg(long = "dry-run")]
    dry_run: bool,
    #[arg(long = "allow-root", value_name = "DIR")]
    allow_roots: Vec<PathBuf>,
    #[arg(long = "allow-secrets")]
    allow_secrets: bool,
    #[arg(long = "redact")]
    redact: bool,
    #[arg(long = "context-file", value_name = "FILE")]
    context_files: Vec<PathBuf>,
    #[arg(long = "silent", short = 'S')]
    silent: bool,
    /// Long-form only: `-v`/`-V` are already claimed by version reporting.
    #[arg(long = "verbose")]
    verbose: bool,
    #[arg(value_name = "INPUT")]
    inputs: Vec<PathBuf>,
}

pub fn parse_cli_args() -> Result<CliArgs, SlopError> {
    parse_cli_args_from(std::env::args_os())
}

pub fn parse_cli_args_from<I, T>(args: I) -> Result<CliArgs, SlopError>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let parsed = RawCliArgs::try_parse_from(args).map_err(|error| match error.kind() {
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
            let _ = error.print();
            std::process::exit(0);
        }
        _ => SlopError::InvalidCliUsage(error.to_string()),
    })?;

    if parsed.inputs.is_empty()
        && !parsed.deslop
        && !parsed.page_close
        && !parsed.page_list
        && !parsed.page_prune
    {
        return Err(SlopError::InvalidCliUsage(
            "at least one input path is required unless -d/--deslop is reading a slop document from stdin"
                .to_string(),
        ));
    }

    if parsed.deslop && parsed.show_output_dir {
        return Err(SlopError::InvalidCliUsage(
            "-d/--deslop cannot be combined with -s/--show".to_string(),
        ));
    }

    let args = cli_args_from_raw(parsed);
    validate_process_modes(&args)?;
    if args.project_graph {
        validate_project_graph_options(&args)?;
    }
    if args.tower_graph {
        validate_tower_graph_options(&args)?;
    }
    if args.page_open {
        validate_page_open_options(&args)?;
    }
    if args.page_add {
        validate_page_add_options(&args)?;
    }

    Ok(args)
}

fn validate_process_modes(args: &CliArgs) -> Result<(), SlopError> {
    let modes = [
        ("--deslop", args.deslop),
        ("--project-graph", args.project_graph),
        ("--tower-graph", args.tower_graph),
        ("--page-open", args.page_open),
        ("--page-add", args.page_add),
        ("--page-close", args.page_close),
        ("--page-list", args.page_list),
        ("--page-prune", args.page_prune),
    ];
    let enabled: Vec<&str> = modes
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect();
    if enabled.len() > 1 {
        return Err(SlopError::InvalidCliUsage(format!(
            "{} cannot be combined with {}",
            enabled[0], enabled[1]
        )));
    }
    Ok(())
}

/// `--project-graph` reads its inputs as a way of naming repositories, not as a
/// payload. Every flag that shapes a bundle is therefore meaningless here, and
/// meaningless flags are rejected rather than ignored: silently dropping
/// `--match` would let someone believe they had scoped a graph they had not.
fn validate_project_graph_options(args: &CliArgs) -> Result<(), SlopError> {
    let mut unsupported = Vec::new();

    if args.deslop {
        unsupported.push("--deslop");
    }
    if args.recursive {
        unsupported.push("-r/--recursive (a project graph always covers the whole repository)");
    }
    if !args.matches.is_empty() {
        unsupported.push("--match");
    }
    if !args.seeds.is_empty() {
        unsupported.push("--seed");
    }
    if !args.symbols.is_empty() {
        unsupported.push("--symbol");
    }
    if args.task.is_some() {
        unsupported.push("--task");
    }
    if args.hops.is_some() {
        unsupported.push("--hops");
    }
    if args.top_k.is_some() {
        unsupported.push("--top-k");
    }
    if args.explain_selection {
        unsupported.push("--explain-selection");
    }
    if !args.context_files.is_empty() {
        unsupported.push("--context-file");
    }
    if !args.exclude.is_empty() {
        unsupported.push("-x/--exclude");
    }
    if args.include_graph {
        unsupported.push("-g/--include-graph (there is no slop here to attach a graph to)");
    }
    if args.max_slop_bytes.is_some() {
        unsupported.push("--max-slop-bytes");
    }
    if args.dry_run {
        unsupported.push("--dry-run");
    }
    if args.redact {
        unsupported.push("--redact");
    }
    if args.allow_secrets {
        unsupported.push("--allow-secrets");
    }

    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(SlopError::InvalidCliUsage(format!(
            "--project-graph builds a graph and bundles nothing, so it cannot use: {}",
            unsupported.join(", ")
        )))
    }
}

fn validate_tower_graph_options(args: &CliArgs) -> Result<(), SlopError> {
    let mut unsupported = Vec::new();
    if !args.matches.is_empty() {
        unsupported.push("--match");
    }
    if !args.seeds.is_empty() {
        unsupported.push("--seed");
    }
    if !args.symbols.is_empty() {
        unsupported.push("--symbol");
    }
    if args.task.is_some() {
        unsupported.push("--task");
    }
    if args.hops.is_some() {
        unsupported.push("--hops");
    }
    if args.top_k.is_some() {
        unsupported.push("--top-k");
    }
    if args.explain_selection {
        unsupported.push("--explain-selection");
    }
    if !args.context_files.is_empty() {
        unsupported.push("--context-file");
    }
    if !args.exclude.is_empty() {
        unsupported.push("-x/--exclude");
    }
    if args.include_graph {
        unsupported.push("-g/--include-graph");
    }
    if args.max_slop_bytes.is_some() {
        unsupported.push("--max-slop-bytes");
    }
    if args.dry_run {
        unsupported.push("--dry-run");
    }
    if args.redact {
        unsupported.push("--redact");
    }
    if args.allow_secrets {
        unsupported.push("--allow-secrets");
    }
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(SlopError::InvalidCliUsage(format!(
            "--tower-graph builds a relevance graph and bundles nothing, so it cannot use: {}",
            unsupported.join(", ")
        )))
    }
}

fn validate_page_open_options(args: &CliArgs) -> Result<(), SlopError> {
    let mut unsupported = Vec::new();
    if !args.matches.is_empty() {
        unsupported.push("--match");
    }
    if !args.seeds.is_empty() {
        unsupported.push("--seed");
    }
    if !args.symbols.is_empty() {
        unsupported.push("--symbol");
    }
    if args.hops.is_some() {
        unsupported.push("--hops");
    }
    if args.top_k.is_some() {
        unsupported.push("--top-k");
    }
    if args.explain_selection {
        unsupported.push("--explain-selection");
    }
    if !args.context_files.is_empty() {
        unsupported.push("--context-file");
    }
    if !args.exclude.is_empty() {
        unsupported.push("-x/--exclude");
    }
    if args.include_graph {
        unsupported.push("-g/--include-graph");
    }
    if args.max_slop_bytes.is_some() {
        unsupported.push("--max-slop-bytes");
    }
    if args.dry_run {
        unsupported.push("--dry-run");
    }
    if !args.allow_roots.is_empty() {
        unsupported.push("--allow-root");
    }
    if args.output_dir.is_some() || args.slop_to.is_some() {
        unsupported.push("--output/--slop-to");
    }
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(SlopError::InvalidCliUsage(format!(
            "--page-open creates a context page, so it cannot use: {}",
            unsupported.join(", ")
        )))
    }
}

fn validate_page_add_options(args: &CliArgs) -> Result<(), SlopError> {
    let mut unsupported = Vec::new();
    if args.recursive {
        unsupported.push("-r/--recursive");
    }
    if args.respect_gitignore {
        unsupported.push("--respect-gitignore");
    }
    if !args.matches.is_empty() {
        unsupported.push("--match");
    }
    if !args.seeds.is_empty() {
        unsupported.push("--seed");
    }
    if !args.symbols.is_empty() {
        unsupported.push("--symbol");
    }
    if args.task.is_some() {
        unsupported.push("--task");
    }
    if args.hops.is_some() {
        unsupported.push("--hops");
    }
    if args.top_k.is_some() {
        unsupported.push("--top-k");
    }
    if args.explain_selection {
        unsupported.push("--explain-selection");
    }
    if !args.context_files.is_empty() {
        unsupported.push("--context-file");
    }
    if !args.exclude.is_empty() {
        unsupported.push("-x/--exclude");
    }
    if args.include_graph {
        unsupported.push("-g/--include-graph");
    }
    if args.max_slop_bytes.is_some() {
        unsupported.push("--max-slop-bytes");
    }
    if args.dry_run {
        unsupported.push("--dry-run");
    }
    if !args.allow_roots.is_empty() {
        unsupported.push("--allow-root");
    }
    if args.output_dir.is_some() || args.slop_to.is_some() {
        unsupported.push("--output/--slop-to");
    }
    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(SlopError::InvalidCliUsage(format!(
            "--page-add extends a context page, so it cannot use: {}",
            unsupported.join(", ")
        )))
    }
}

/// Parse options attached to a slopheap closing directive with the exact same
/// Clap definition as the top-level command. The heap root supplies the
/// positional input that a normal slop invocation requires.
pub(crate) fn parse_slopheap_options(options: &str, root: &Path) -> Result<CliArgs, SlopError> {
    let tokens = shell_words::split(options).map_err(|error| {
        SlopError::InvalidCliUsage(format!("invalid slopheap options: {error}"))
    })?;
    let mut args = Vec::with_capacity(tokens.len() + 2);
    args.push(std::ffi::OsString::from("slop"));
    args.extend(tokens.into_iter().map(std::ffi::OsString::from));
    args.push(root.as_os_str().to_os_string());

    let parsed = RawCliArgs::try_parse_from(args).map_err(|error| {
        SlopError::InvalidCliUsage(format!("invalid slopheap options: {error}"))
    })?;
    let args = cli_args_from_raw(parsed);
    validate_slopheap_options(&args)?;
    Ok(args)
}

fn validate_slopheap_options(args: &CliArgs) -> Result<(), SlopError> {
    let mut unsupported = Vec::new();
    if args.deslop {
        unsupported.push("--deslop");
    }
    if args.show_output_dir {
        unsupported.push("--show");
    }
    if args.output_dir.is_some() {
        unsupported.push("--output");
    }
    if args.ignore_slopignore {
        unsupported.push("--ignore-slopignore");
    }
    if args.slop_to.is_some() {
        unsupported.push("--slop-to");
    }
    if args.dry_run {
        unsupported.push("--dry-run");
    }
    if !args.allow_roots.is_empty() {
        unsupported.push("--allow-root");
    }
    if args.silent {
        unsupported.push("--silent");
    }
    if args.verbose {
        unsupported.push("--verbose");
    }
    if args.recursive {
        unsupported.push("--recursive (slopheap includes are already recursive)");
    }
    if args.project_graph {
        unsupported.push("--project-graph");
    }
    if args.tower_graph {
        unsupported.push("--tower-graph");
    }
    if args.inputs.len() != 1 {
        unsupported.push("positional inputs");
    }

    if unsupported.is_empty() {
        Ok(())
    } else {
        Err(SlopError::InvalidCliUsage(format!(
            "slopheap options cannot control the outer process: {}",
            unsupported.join(", ")
        )))
    }
}

fn cli_args_from_raw(parsed: RawCliArgs) -> CliArgs {
    CliArgs {
        deslop: parsed.deslop,
        show_output_dir: parsed.show_output_dir,
        output_dir: parsed.output_dir,
        recursive: parsed.recursive,
        inputs: parsed.inputs,
        exclude: parsed.exclude,
        respect_gitignore: parsed.respect_gitignore,
        ignore_slopignore: parsed.ignore_slopignore,
        include_graph: parsed.include_graph,
        project_graph: parsed.project_graph,
        tower_graph: parsed.tower_graph,
        page_open: parsed.page_open,
        page_add: parsed.page_add,
        page_close: parsed.page_close,
        page_list: parsed.page_list,
        page_prune: parsed.page_prune,
        page_id: parsed.page_id,
        older_than: parsed.older_than,
        slop_to: parsed.slop_to,
        graph_format: parsed.graph_format,
        graph_map_tokens: parsed.graph_map_tokens,
        matches: parsed.match_terms,
        seeds: parsed.seeds,
        hops: parsed.hops,
        symbols: parsed.symbols,
        task: parsed.task,
        top_k: parsed.top_k,
        max_slop_bytes: parsed.max_slop_bytes,
        reindex: parsed.reindex,
        explain_selection: parsed.explain_selection,
        dry_run: parsed.dry_run,
        allow_roots: parsed.allow_roots,
        allow_secrets: parsed.allow_secrets,
        redact: parsed.redact,
        context_files: parsed.context_files,
        silent: parsed.silent,
        verbose: parsed.verbose,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_cli_args_from, parse_slopheap_options};

    #[test]
    fn rejects_missing_inputs() {
        let result = parse_cli_args_from(["slop"]);
        let error = result.expect_err("expected missing input failure");
        assert!(error.to_string().contains("required"));
    }

    #[test]
    fn allows_deslop_without_inputs_for_stdin() {
        let result = parse_cli_args_from(["slop", "-d"]).expect("stdin deslop should parse");
        assert!(result.deslop);
        assert!(result.inputs.is_empty());
    }

    #[test]
    fn rejects_deslop_show_combination() {
        let result = parse_cli_args_from(["slop", "-d", "-s", "file.txt"]);
        let error = result.expect_err("expected invalid flag combination");
        assert!(error.to_string().contains("cannot be combined"));
    }

    #[test]
    fn parses_include_graph_flag() {
        let result = parse_cli_args_from(["slop", "-g", "file.txt"]).expect("should parse");
        assert!(result.include_graph);
    }

    #[test]
    fn parses_respect_gitignore_flag() {
        let result =
            parse_cli_args_from(["slop", "--respect-gitignore", "file.txt"]).expect("should parse");
        assert!(result.respect_gitignore);
    }

    #[test]
    fn respect_gitignore_defaults_to_false() {
        let result = parse_cli_args_from(["slop", "file.txt"]).expect("should parse");
        assert!(!result.respect_gitignore);
    }

    #[test]
    fn parses_ignore_slopignore_flag() {
        let result =
            parse_cli_args_from(["slop", "--ignore-slopignore", "file.txt"]).expect("should parse");
        assert!(result.ignore_slopignore);
    }

    #[test]
    fn ignore_slopignore_defaults_to_false() {
        let result = parse_cli_args_from(["slop", "file.txt"]).expect("should parse");
        assert!(!result.ignore_slopignore);
    }

    #[test]
    fn parses_graph_format_and_tokens() {
        let result = parse_cli_args_from([
            "slop",
            "--graph-format",
            "dot",
            "--graph-map-tokens",
            "4096",
            "file.txt",
        ])
        .expect("should parse");
        assert_eq!(result.graph_format.as_deref(), Some("dot"));
        assert_eq!(result.graph_map_tokens, Some(4096));
    }

    #[test]
    fn parses_verbose_flag() {
        let result = parse_cli_args_from(["slop", "--verbose", "file.txt"]).expect("should parse");
        assert!(result.verbose);
    }

    #[test]
    fn verbose_defaults_to_false() {
        let result = parse_cli_args_from(["slop", "file.txt"]).expect("should parse");
        assert!(!result.verbose);
    }

    #[test]
    fn parses_slop_to_flag() {
        let result = parse_cli_args_from(["slop", "--slop-to", "/tmp/out", "file.txt"])
            .expect("should parse");
        assert_eq!(
            result.slop_to.as_deref(),
            Some(std::path::Path::new("/tmp/out"))
        );
    }

    #[test]
    fn parses_project_graph_flag() {
        let result = parse_cli_args_from(["slop", "--project-graph", "src"]).expect("should parse");
        assert!(result.project_graph);
        assert_eq!(result.inputs, vec![std::path::PathBuf::from("src")]);
    }

    #[test]
    fn project_graph_defaults_to_false() {
        let result = parse_cli_args_from(["slop", "file.txt"]).expect("should parse");
        assert!(!result.project_graph);
    }

    #[test]
    fn project_graph_still_requires_an_input_to_locate_the_repository() {
        let error = parse_cli_args_from(["slop", "--project-graph"])
            .expect_err("nothing names a repository");
        assert!(error.to_string().contains("required"), "{error}");
    }

    #[test]
    fn project_graph_rejects_bundle_shaping_flags_instead_of_ignoring_them() {
        // The dangerous failure is the silent one: a user who passes --match
        // and believes the resulting graph was scoped by it.
        let error = parse_cli_args_from(["slop", "--project-graph", "--match", "login", "src"])
            .expect_err("selection cannot scope a project graph");
        assert!(error.to_string().contains("--match"), "{error}");
    }

    #[test]
    fn project_graph_rejects_recursive_with_an_explanation() {
        let error = parse_cli_args_from(["slop", "--project-graph", "-r", "src"])
            .expect_err("recursion is not a thing a whole-repo graph can do");
        assert!(error.to_string().contains("whole repository"), "{error}");
    }

    #[test]
    fn project_graph_accepts_output_and_rebuild_flags() {
        let result = parse_cli_args_from([
            "slop",
            "--project-graph",
            "--reindex",
            "--slop-to",
            "/tmp/out",
            "--silent",
            "src",
        ])
        .expect("these all still make sense for a graph run");
        assert!(result.project_graph);
        assert!(result.reindex);
        assert!(result.silent);
    }

    #[test]
    fn a_slopheap_cannot_turn_the_outer_run_into_a_graph_build() {
        let error = parse_slopheap_options("--project-graph", std::path::Path::new("/tmp/project"))
            .expect_err("heap options must not seize the process mode");
        assert!(
            error
                .to_string()
                .contains("cannot control the outer process")
        );
    }

    #[test]
    fn tower_graph_allows_recursive_seeds_but_rejects_bundle_selection() {
        let args = parse_cli_args_from(["slop", "--tower-graph", "-r", "src"])
            .expect("recursive tower seed set is valid");
        assert!(args.tower_graph);
        assert!(args.recursive);

        let error = parse_cli_args_from(["slop", "--tower-graph", "--match", "login", "src"])
            .expect_err("selection cannot scope a tower graph");
        assert!(
            error
                .to_string()
                .contains("--tower-graph builds a relevance graph")
        );
        assert!(error.to_string().contains("--match"));
    }

    #[test]
    fn page_close_and_list_do_not_require_inputs() {
        assert!(
            parse_cli_args_from(["slop", "--page-close"])
                .expect("close parses")
                .page_close
        );
        assert!(
            parse_cli_args_from(["slop", "--page-list"])
                .expect("list parses")
                .page_list
        );
    }

    #[test]
    fn process_modes_are_mutually_exclusive() {
        let error = parse_cli_args_from(["slop", "--tower-graph", "--page-open", "src"])
            .expect_err("two process modes conflict");
        assert!(error.to_string().contains("cannot be combined"));
    }

    #[test]
    fn parses_quoted_slopheap_options_with_the_standard_cli_definition() {
        let root = std::path::Path::new("/tmp/project");
        let result = parse_slopheap_options(
            "-g --graph-map-tokens 4096 --match 'login flow' -x '*.tmp'",
            root,
        )
        .expect("heap options should parse");

        assert!(result.include_graph);
        assert_eq!(result.graph_map_tokens, Some(4096));
        assert_eq!(result.matches, vec!["login flow"]);
        assert_eq!(result.exclude, vec!["*.tmp"]);
        assert_eq!(result.inputs, vec![root]);
    }

    #[test]
    fn rejects_process_mode_options_in_slopheaps() {
        let error = parse_slopheap_options("--dry-run", std::path::Path::new("/tmp/project"))
            .expect_err("process mode should not be silently ignored");
        assert!(
            error
                .to_string()
                .contains("cannot control the outer process")
        );
    }
}
