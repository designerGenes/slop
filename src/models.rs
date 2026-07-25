use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub deslop: bool,
    pub show_output_dir: bool,
    pub output_dir: Option<PathBuf>,
    pub recursive: bool,
    pub inputs: Vec<PathBuf>,
    pub exclude: Vec<String>,
    pub respect_gitignore: bool,
    pub include_graph: bool,
    pub slop_to: Option<PathBuf>,
    pub graph_format: Option<String>,
    pub graph_map_tokens: Option<usize>,
    pub matches: Vec<String>,
    pub seeds: Vec<PathBuf>,
    pub hops: Option<usize>,
    pub symbols: Vec<String>,
    pub task: Option<String>,
    pub top_k: Option<usize>,
    pub max_slop_bytes: Option<usize>,
    pub reindex: bool,
    pub explain_selection: bool,
    pub dry_run: bool,
    pub allow_roots: Vec<PathBuf>,
    pub allow_secrets: bool,
    pub redact: bool,
    pub context_files: Vec<PathBuf>,
    pub silent: bool,
    pub verbose: bool,
}

/// Why a path was left out of a slop.
///
/// `SKIP_DIRS` (`.git`, `node_modules`, `target`, ...) is deliberately absent:
/// those prunes are structural and reporting them would bury the signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoreReason {
    /// Matched a pattern in the repo's `.slopignore`.
    SlopIgnore,
    /// Matched a `-x/--exclude` pattern given on the command line.
    Exclude,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoredEntry {
    pub path: PathBuf,
    pub is_dir: bool,
    pub reason: IgnoreReason,
}

/// Result of a directory walk: what slop will bundle, and what it skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkReport {
    pub files: Vec<PathBuf>,
    pub ignored: Vec<IgnoredEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    pub original_absolute_path: PathBuf,
    pub file_name: String,
    pub name_token: String,
    pub contents: String,
    pub logical_line_count: usize,
    pub trailing_newline: bool,
    pub base_sha: Option<String>,
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoupPartialRange {
    pub start_line: usize,
    pub end_line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoupBlock {
    pub original_absolute_path: PathBuf,
    pub partial_range: Option<SoupPartialRange>,
    pub logical_line_count: usize,
    pub trailing_newline: bool,
    pub content_lines: Vec<String>,
    pub base_sha: Option<String>,
    pub read_only: bool,
    pub block_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoupMetaBlock {
    pub label: String,
    pub kind: String,
    pub format: String,
    pub line_count: usize,
    pub readonly: bool,
    pub content_lines: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SoupDocument {
    pub meta_blocks: Vec<SoupMetaBlock>,
    pub blocks: Vec<SoupBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoupMatchResult {
    One(PathBuf),
    None,
    Ambiguous(Vec<PathBuf>),
}
