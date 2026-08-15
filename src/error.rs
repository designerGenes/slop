use std::io;
use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SlopError {
    #[error("invalid CLI usage: {0}")]
    InvalidCliUsage(String),

    #[error("input path does not exist: {0}")]
    MissingInputPath(PathBuf),

    #[error("input paths expanded to zero files")]
    InputExpandedToZeroFiles,

    #[error("unsupported file type encountered: {0}")]
    UnsupportedFileType(PathBuf),

    #[error("file is not valid UTF-8: {0}")]
    Utf8DecodeFailure(PathBuf),

    #[error("failed to resolve home directory")]
    HomeDirectoryResolutionFailure,

    #[error("failed to create directory {path}: {source}")]
    DirectoryCreationFailure { path: PathBuf, source: io::Error },

    #[error("failed to read file {path}: {source}")]
    FileReadFailure { path: PathBuf, source: io::Error },

    #[error("failed to write file {path}: {source}")]
    FileWriteFailure { path: PathBuf, source: io::Error },

    #[error("terminal interaction failed: {0}")]
    TerminalInteractionFailure(io::Error),

    #[error("manual deslop cancelled")]
    ManualDeslopCancelled,

    #[error("slop parse failure: {0}")]
    SoupParseFailure(String),

    #[error(
        "no matching slop file found in {slop_dir} for selectors: {selectors}",
        slop_dir = .slop_dir.display(),
        selectors = format_paths(.selectors)
    )]
    NoMatchingSoupFile {
        selectors: Vec<PathBuf>,
        slop_dir: PathBuf,
    },

    #[error(
        "multiple slop files matched selectors: {paths}",
        paths = format_paths(.paths)
    )]
    AmbiguousSoupFileMatch { paths: Vec<PathBuf> },

    #[error("failed to open output directory {directory}: {message}", directory = .directory.display())]
    OpenDirectoryFailure { directory: PathBuf, message: String },

    #[error(
        "slop file written to {slop_file}, but failed to open output directory {directory}: {message}",
        slop_file = .slop_file.display(),
        directory = .directory.display()
    )]
    OpenDirectoryAfterWriteFailed {
        slop_file: PathBuf,
        directory: PathBuf,
        message: String,
    },

    #[error("config error: {0}")]
    ConfigError(String),

    #[error("git repository not found: {0}")]
    GitRepoNotFound(String),

    #[error("repomap generation failed: {0}")]
    RepoMapGenerationFailure(String),

    #[error("--task supplied but config.allow_fuzzy_task is false; use --match/--seed/--symbol, or enable allow_fuzzy_task")]
    FuzzyTaskDisabled,

    #[error("index build failure: {0}")]
    IndexBuildFailure(String),

    #[error("retrieval query failure: {0}")]
    RetrievalQueryFailure(String),

    #[error("slop budget exceeded: highest-priority file {path} ({bytes} bytes) + map already exceed cap ({cap} bytes); raise --max-slop-bytes, narrow selectors, or use partial blocks")]
    SoupBudgetExceeded { path: PathBuf, bytes: usize, cap: usize },

    #[error("base SHA drift for {path}: expected {expected}, got {actual}; file changed since slopification")]
    BaseShaDrift { path: PathBuf, expected: String, actual: String },

    #[error("write target {path} escapes allowed roots {allowed_roots}", allowed_roots = format_paths(.allowed_roots))]
    WriteOutsideAllowedRoot { path: PathBuf, allowed_roots: Vec<PathBuf> },

    #[error("unexpected #SLOP_META block in returned slop; AI must not emit meta blocks")]
    UnexpectedMetaInReturn,

    #[error("secrets detected in slop content: {findings_summary}")]
    SecretsDetected { findings_summary: String },
}

fn format_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
