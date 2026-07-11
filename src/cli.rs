use std::path::PathBuf;

use clap::Parser;

use crate::error::SlopError;
use crate::models::CliArgs;

#[derive(Debug, Parser)]
#[command(
    name = "slop",
    version,
    about = "Combine files into a markdown slop"
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
    #[arg(short = 'g', long = "include-graph")]
    include_graph: bool,
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
    #[arg(value_name = "INPUT", required = true)]
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
    let parsed = RawCliArgs::try_parse_from(args)
        .map_err(|error| SlopError::InvalidCliUsage(error.to_string()))?;

    if parsed.inputs.is_empty() {
        return Err(SlopError::InvalidCliUsage(
            "at least one input path is required".to_string(),
        ));
    }

    if parsed.deslop && parsed.show_output_dir {
        return Err(SlopError::InvalidCliUsage(
            "-d/--deslop cannot be combined with -s/--show".to_string(),
        ));
    }

    Ok(CliArgs {
        deslop: parsed.deslop,
        show_output_dir: parsed.show_output_dir,
        output_dir: parsed.output_dir,
        recursive: parsed.recursive,
        inputs: parsed.inputs,
        exclude: parsed.exclude,
        respect_gitignore: parsed.respect_gitignore,
        include_graph: parsed.include_graph,
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
    })
}

#[cfg(test)]
mod tests {
    use super::parse_cli_args_from;

    #[test]
    fn rejects_missing_inputs() {
        let result = parse_cli_args_from(["slop"]);
        let error = result.expect_err("expected missing input failure");
        assert!(error.to_string().contains("required"));
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
        let result = parse_cli_args_from(["slop", "--respect-gitignore", "file.txt"])
            .expect("should parse");
        assert!(result.respect_gitignore);
    }

    #[test]
    fn respect_gitignore_defaults_to_false() {
        let result = parse_cli_args_from(["slop", "file.txt"]).expect("should parse");
        assert!(!result.respect_gitignore);
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
    fn parses_slop_to_flag() {
        let result =
            parse_cli_args_from(["slop", "--slop-to", "/tmp/out", "file.txt"])
                .expect("should parse");
        assert_eq!(
            result.slop_to.as_deref(),
            Some(std::path::Path::new("/tmp/out"))
        );
    }
}
