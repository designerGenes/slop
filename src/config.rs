use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::SlopError;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub connect_with_downloads_watcher: bool,
    pub auto_deslop: bool,
    pub warn_before_overwriting: bool,
    pub to_deslop_folder: Option<PathBuf>,
    pub slopified_folder: Option<PathBuf>,
    pub include_graph: bool,
    pub respect_gitignore: bool,
    #[serde(alias = "ignore_slopignore_for_full_statement")]
    pub skip_slopignore_for_full_statement: bool,
    pub graph_map_tokens: usize,
    pub graph_format: String,
    pub graph_force_include_supertypes: bool,
    pub index_dir: Option<PathBuf>,
    pub selection_default_hops: usize,
    pub top_k: usize,
    pub max_slop_bytes: usize,
    pub allow_fuzzy_task: bool,
    pub selection_provenance: bool,
    pub selection_provenance_max_bytes: usize,
    pub secret_scan: String,
    pub redact_secrets: bool,
    pub secret_rules_path: Option<PathBuf>,
    pub graph_token_model: String,
    /// Root of the persisted graph store. Defaults to
    /// `$HOME/.cache/slop/graphs`.
    pub graph_store_dir: Option<PathBuf>,
    /// Ceiling on files walked when building a project graph.
    pub graph_max_files: usize,
    /// Louvain resolution. Above 1.0 splits into more, smaller modules.
    pub graph_community_resolution: f64,
    /// How much a co-change edge counts relative to a symbol edge when
    /// clustering. Below 1.0: a proven reference beats a shared commit.
    pub graph_cochange_weight: f64,
    /// Commits of history to mine. 0 disables co-change entirely.
    pub graph_cochange_commits: usize,
    /// Commits touching more files than this are sweeps, not coupling.
    pub graph_cochange_max_files_per_commit: usize,
    /// Pairs seen in fewer commits than this are coincidence.
    pub graph_cochange_min_commits: usize,
    pub graph_cochange_max_edges: usize,
    /// Write a readable graph artifact alongside the cached JSON.
    pub graph_emit_artifact: bool,
    pub tower_restart_alpha: f64,
    pub tower_rwr_iterations: usize,
    pub tower_rwr_epsilon: f64,
    pub tower_community_bonus: f64,
    pub tower_tier1_mass_fraction: f64,
    pub tower_tier2_mass_fraction: f64,
    pub tower_tier3_min_score: f64,
    pub pages_dir: Option<PathBuf>,
    pub page_prune_after: String,
    pub page_tier1_include: bool,
    pub verbose_output: bool,
    pub deslop_cache: bool,
    pub deslop_cache_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connect_with_downloads_watcher: false,
            auto_deslop: false,
            warn_before_overwriting: false,
            to_deslop_folder: None,
            slopified_folder: None,
            include_graph: false,
            respect_gitignore: false,
            skip_slopignore_for_full_statement: false,
            graph_map_tokens: 2048,
            graph_format: "repomap".to_string(),
            graph_force_include_supertypes: true,
            index_dir: None,
            selection_default_hops: 1,
            top_k: 12,
            max_slop_bytes: 1_048_576,
            allow_fuzzy_task: true,
            selection_provenance: false,
            selection_provenance_max_bytes: 2048,
            secret_scan: "warn".to_string(),
            redact_secrets: false,
            secret_rules_path: None,
            graph_token_model: "o200k_base".to_string(),
            graph_store_dir: None,
            graph_max_files: 20_000,
            graph_community_resolution: 1.0,
            graph_cochange_weight: 0.5,
            graph_cochange_commits: 500,
            graph_cochange_max_files_per_commit: 40,
            graph_cochange_min_commits: 2,
            graph_cochange_max_edges: 4_000,
            graph_emit_artifact: true,
            tower_restart_alpha: 0.2,
            tower_rwr_iterations: 30,
            tower_rwr_epsilon: 1e-9,
            tower_community_bonus: 0.5,
            tower_tier1_mass_fraction: 0.60,
            tower_tier2_mass_fraction: 0.85,
            tower_tier3_min_score: 1e-6,
            pages_dir: None,
            page_prune_after: "7d".to_string(),
            page_tier1_include: true,
            verbose_output: false,
            // Off by default: deslop idempotency now comes from comparing the
            // block against the file on disk, which is exact and stateless.
            // The ledger is an opt-in overlay for people who want a returned
            // block applied at most once ever, even after the file changes.
            deslop_cache: false,
            deslop_cache_path: None,
        }
    }
}

pub fn load_config() -> Config {
    let Some(config_path) = default_config_path() else {
        return Config::default();
    };
    load_config_from(&config_path).unwrap_or_default()
}

pub fn load_config_from(path: &Path) -> Result<Config, SlopError> {
    let contents = fs::read_to_string(path)
        .map_err(|error| SlopError::ConfigError(format!("{}: {}", path.display(), error)))?;
    serde_yaml::from_str(&contents)
        .map_err(|error| SlopError::ConfigError(format!("{}: {}", path.display(), error)))
}

pub fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".config").join("slop").join("config.yaml"))
}

pub fn default_config_yaml() -> String {
    format!(
        "# slop configuration\n\
         # Settings here are scanned at every invocation and can be overridden\n\
         # by command-line flags.\n\n\
         # Connect with Sharktopus (downloads watcher). If true, slop will\n\
         # add/verify a rule to automatically move .slop.md files downloaded to\n\
         # $HOME/Downloads into the \"to deslop\" folder.\n\
         connect_with_downloads_watcher: {connect_watcher}\n\n\
         # If true, slop will automatically de-slop any Soup files in the\n\
         # \"to deslop\" folder. If false, they are only moved there and the\n\
         # user must manually run slop to de-slop them.\n\
         auto_deslop: {auto_deslop}\n\n\
         # If true, slop will warn before overwriting existing files during\n\
         # de-slopification.\n\
         warn_before_overwriting: {warn_overwrite}\n\n\
         # Path to the folder where Soup files are moved for de-slopification.\n\
         # Defaults to $HOME/.slop/to_deslop\n\
         to_deslop_folder: {to_deslop}\n\n\
         # Path to the folder where Soupified files are saved. Defaults to\n\
         # $HOME/.slop/slopified. Can be overridden at invocation with\n\
         # --slop-to.\n\
         slopified_folder: {slopified}\n\n\
         # Include a code-graph metadata block when sloping. Override per-run\n\
         # with --include-graph.\n\
         include_graph: {include_graph}\n\n\
         # If true, slop will skip any files or folders matched by the\n\
         # target repo's .gitignore when walking a directory. Override\n\
         # per-run with --respect-gitignore.\n\
         respect_gitignore: {respect_gitignore}\n\n\
         # If true, an invocation naming only explicit files skips the repo's\n\
         # .slopignore (including its slopinclude directives). Override per-run\n\
         # with --ignore-slopignore.\n\
         skip_slopignore_for_full_statement: {skip_slopignore}\n\n\
         # RepoMapper --map-tokens; compactness lever for the graph.\n\
         graph_map_tokens: {graph_tokens}\n\n\
         # Graph format: repomap | dot | json | mermaid\n\
         graph_format: {graph_format}\n\n\
         # Force-include declared protocols/superclasses of seed files.\n\
         graph_force_include_supertypes: {force_supertypes}\n\n\
         # Directory for the selection full-text index. Defaults to\n\
         # $HOME/.cache/slop/index. Lives outside any repo tree.\n\
         index_dir: {index_dir}\n\n\
         # Default BFS radius around --seed files.\n\
         selection_default_hops: {sel_hops}\n\n\
         # Max files selected by --match/--task/--symbol/--seed.\n\
         top_k: {top_k}\n\n\
         # Hard ceiling on serialized slop bytes (1 MiB default).\n\
         max_slop_bytes: {max_slop_bytes}\n\n\
         # If false, --task is rejected (deterministic-only mode).\n\
         allow_fuzzy_task: {allow_fuzzy}\n\n\
         # Emit a selection provenance #SLOP_META block.\n\
         selection_provenance: {sel_prov}\n\n\
         # Max bytes for the provenance block.\n\
         selection_provenance_max_bytes: {sel_prov_max}\n\n\
         # Secret scan mode: off | warn | block. 'off' disables all scanning;\n\
         # 'warn' (default) prints warnings but never blocks; 'block' refuses\n\
         # slops containing high-confidence secret patterns (private keys,\n\
         # AWS/Google/Stripe tokens, JWTs, etc.). Override per-run with\n\
         # --allow-secrets or --redact.\n\
         secret_scan: {secret_scan}\n\n\
         # Root of the persisted graph store used by --project-graph.\n\
         # Defaults to $HOME/.cache/slop/graphs. Regenerable: safe to delete.\n\
         graph_store_dir: {graph_store_dir}\n\n\
         # Ceiling on files walked when building a project graph.\n\
         graph_max_files: {graph_max_files}\n\n\
         # Louvain resolution for module detection. >1.0 gives more, smaller\n\
         # modules; <1.0 gives fewer, larger ones.\n\
         graph_community_resolution: {graph_community_resolution}\n\n\
         # Weight of a co-change edge relative to a symbol edge when\n\
         # clustering. A proven reference is stronger evidence than a shared\n\
         # commit, so this sits below 1.0.\n\
         graph_cochange_weight: {graph_cochange_weight}\n\n\
         # Commits of git history mined for co-change signal. 0 disables it.\n\
         graph_cochange_commits: {graph_cochange_commits}\n\n\
         # Commits touching more files than this are treated as sweeps\n\
         # (formatting, renames, license headers) and contribute nothing.\n\
         graph_cochange_max_files_per_commit: {graph_cochange_max_files}\n\n\
         # A pair must co-occur in at least this many commits to count.\n\
         graph_cochange_min_commits: {graph_cochange_min_commits}\n\n\
         # Ceiling on retained co-change edges, highest weight first.\n\
         graph_cochange_max_edges: {graph_cochange_max_edges}\n\n\
         # Write a readable .project-graph.md next to your slop files in\n\
         # addition to the cached JSON. The cache is the source of truth.\n\
         graph_emit_artifact: {graph_emit_artifact}\n\n\
         # Personalized relevance walk settings for --tower-graph.\n\
         tower_restart_alpha: {tower_restart_alpha}\n\
         tower_rwr_iterations: {tower_rwr_iterations}\n\
         tower_rwr_epsilon: {tower_rwr_epsilon}\n\
         tower_community_bonus: {tower_community_bonus}\n\
         tower_tier1_mass_fraction: {tower_tier1_mass_fraction}\n\
         tower_tier2_mass_fraction: {tower_tier2_mass_fraction}\n\
         tower_tier3_min_score: {tower_tier3_min_score}\n\n\
         # Context-page state is durable agent work, not cache data.\n\
         pages_dir: {pages_dir}\n\
         page_prune_after: {page_prune_after}\n\
         page_tier1_include: {page_tier1_include}\n\n\
         # Print the ignored-file annotations in the slopify tree. Override\n\
         # per-run with --verbose.\n\
         verbose_output: {verbose_output}\n\n\
         # Remember which returned slop blocks have already been deslopped and\n\
         # skip them on later runs, even if the target file has since changed.\n\
         # Deslop is already idempotent without this: a block whose content\n\
         # matches the file on disk is never rewritten. Turn this on only if\n\
         # you want a block applied at most once, ever.\n\
         deslop_cache: {deslop_cache}\n\n\
         # Where that ledger lives. Defaults to\n\
         # $HOME/.slop/.slop_blocks_cache. The SLOP_DESLOP_CACHE environment\n\
         # variable overrides this; set it to 'off' to disable caching for a\n\
         # single run.\n\
         deslop_cache_path: {deslop_cache_path}\n",
        connect_watcher = false,
        auto_deslop = false,
        warn_overwrite = false,
        to_deslop = "~/.slop/to_deslop",
        slopified = "~/.slop/slopified",
        include_graph = false,
        respect_gitignore = false,
        skip_slopignore = false,
        graph_tokens = 2048,
        graph_format = "repomap",
        force_supertypes = true,
        index_dir = "~/.cache/slop/index",
        sel_hops = 1,
        top_k = 12,
        max_slop_bytes = 1_048_576,
        allow_fuzzy = true,
        sel_prov = false,
        sel_prov_max = 2048,
        secret_scan = "warn",
        graph_store_dir = "~/.cache/slop/graphs",
        graph_max_files = 20_000,
        graph_community_resolution = 1.0,
        graph_cochange_weight = 0.5,
        graph_cochange_commits = 500,
        graph_cochange_max_files = 40,
        graph_cochange_min_commits = 2,
        graph_cochange_max_edges = 4_000,
        graph_emit_artifact = true,
        tower_restart_alpha = 0.2,
        tower_rwr_iterations = 30,
        tower_rwr_epsilon = 1e-9,
        tower_community_bonus = 0.5,
        tower_tier1_mass_fraction = 0.60,
        tower_tier2_mass_fraction = 0.85,
        tower_tier3_min_score = 1e-6,
        pages_dir = "~/.slop/pages",
        page_prune_after = "7d",
        page_tier1_include = true,
        verbose_output = false,
        deslop_cache = false,
        deslop_cache_path = "~/.slop/.slop_blocks_cache",
    )
}

pub fn ensure_config_dir() -> Result<PathBuf, SlopError> {
    let config_path = default_config_path().ok_or(SlopError::HomeDirectoryResolutionFailure)?;
    let config_dir = config_path
        .parent()
        .ok_or_else(|| SlopError::ConfigError("config path has no parent directory".to_string()))?;

    fs::create_dir_all(config_dir).map_err(|error| SlopError::DirectoryCreationFailure {
        path: config_dir.to_path_buf(),
        source: error,
    })?;

    if !config_path.exists() {
        fs::write(&config_path, default_config_yaml()).map_err(|error| {
            SlopError::FileWriteFailure {
                path: config_path.clone(),
                source: error,
            }
        })?;
    }

    Ok(config_path)
}

pub fn default_to_deslop_folder() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".slop").join("to_deslop"))
}

pub fn default_slopified_folder() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".slop").join("slopified"))
}

pub fn default_index_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".cache").join("slop").join("index"))
}

/// Persisted project and tower graphs. Cache, not state: everything here can be
/// rebuilt from the repository, so it sits beside the selection index rather
/// than in `$HOME/.slop/` where losing a file would cost work.
pub fn default_graph_store_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".cache").join("slop").join("graphs"))
}

pub fn default_pages_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".slop").join("pages"))
}

/// Ledger of already-applied deslop blocks. Lives beside the other slop state
/// in `$HOME/.slop/` rather than `$HOME/.cache/`, so that clearing an OS cache
/// directory cannot silently change deslop behavior.
pub fn default_deslop_cache_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".slop").join(".slop_blocks_cache"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn loads_default_config_when_file_missing() {
        let config = load_config_from(Path::new("/nonexistent/config.yaml"))
            .expect_err("should fail for missing file");
        assert!(config.to_string().contains("config error"));
    }

    #[test]
    fn parses_partial_yaml_with_defaults() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("config.yaml");
        fs::write(&path, "auto_deslop: true\ngraph_map_tokens: 4096\n").expect("write config");

        let config = load_config_from(&path).expect("should parse");
        assert!(config.auto_deslop);
        assert!(!config.connect_with_downloads_watcher);
        assert_eq!(config.graph_map_tokens, 4096);
        assert_eq!(config.graph_format, "repomap");
        assert!(config.graph_force_include_supertypes);
    }

    #[test]
    fn parses_full_yaml() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("config.yaml");
        fs::write(
            &path,
            "connect_with_downloads_watcher: true\n\
             auto_deslop: true\n\
             warn_before_overwriting: true\n\
             to_deslop_folder: /tmp/to_deslop\n\
             slopified_folder: /tmp/slopified\n\
             include_graph: true\n\
             respect_gitignore: true\n\
             graph_map_tokens: 1024\n\
             graph_format: dot\n\
             graph_force_include_supertypes: false\n",
        )
        .expect("write config");

        let config = load_config_from(&path).expect("should parse");
        assert!(config.connect_with_downloads_watcher);
        assert!(config.auto_deslop);
        assert!(config.warn_before_overwriting);
        assert_eq!(
            config.to_deslop_folder,
            Some(PathBuf::from("/tmp/to_deslop"))
        );
        assert_eq!(
            config.slopified_folder,
            Some(PathBuf::from("/tmp/slopified"))
        );
        assert!(config.include_graph);
        assert!(config.respect_gitignore);
        assert!(!config.skip_slopignore_for_full_statement);
        assert_eq!(config.graph_map_tokens, 1024);
        assert_eq!(config.graph_format, "dot");
        assert!(!config.graph_force_include_supertypes);
    }

    #[test]
    fn default_config_has_expected_values() {
        let config = Config::default();
        assert!(!config.connect_with_downloads_watcher);
        assert!(!config.auto_deslop);
        assert!(!config.warn_before_overwriting);
        assert!(config.to_deslop_folder.is_none());
        assert!(config.slopified_folder.is_none());
        assert!(!config.include_graph);
        assert!(!config.respect_gitignore);
        assert!(!config.skip_slopignore_for_full_statement);
        assert_eq!(config.graph_map_tokens, 2048);
        assert_eq!(config.graph_format, "repomap");
        assert!(config.graph_force_include_supertypes);
    }

    #[test]
    fn default_config_yaml_contains_all_keys() {
        let yaml = default_config_yaml();
        assert!(yaml.contains("connect_with_downloads_watcher:"));
        assert!(yaml.contains("auto_deslop:"));
        assert!(yaml.contains("warn_before_overwriting:"));
        assert!(yaml.contains("to_deslop_folder:"));
        assert!(yaml.contains("slopified_folder:"));
        assert!(yaml.contains("include_graph:"));
        assert!(yaml.contains("respect_gitignore:"));
        assert!(yaml.contains("skip_slopignore_for_full_statement:"));
        assert!(yaml.contains("graph_map_tokens:"));
        assert!(yaml.contains("graph_format:"));
        assert!(yaml.contains("graph_force_include_supertypes:"));
        assert!(yaml.contains("secret_scan:"));
        assert!(yaml.contains("verbose_output:"));
        assert!(yaml.contains("deslop_cache:"));
        assert!(yaml.contains("deslop_cache_path:"));
    }

    #[test]
    fn deslop_cache_defaults_to_off_and_lives_under_dot_slop() {
        let config = Config::default();
        assert!(!config.deslop_cache);
        assert!(config.deslop_cache_path.is_none());
        assert!(!config.verbose_output);

        // SAFETY: this test runs single-threaded; mutating HOME is safe.
        unsafe {
            std::env::set_var("HOME", "/home/example");
        }
        assert_eq!(
            default_deslop_cache_path(),
            Some(PathBuf::from("/home/example/.slop/.slop_blocks_cache"))
        );
        unsafe {
            std::env::set_var("HOME", "/tmp");
        }
    }

    #[test]
    fn parses_verbose_and_cache_overrides() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("config.yaml");
        fs::write(
            &path,
            "verbose_output: true\n\
             deslop_cache: true\n\
             deslop_cache_path: /tmp/blocks\n",
        )
        .expect("write config");

        let config = load_config_from(&path).expect("should parse");
        assert!(config.verbose_output);
        assert!(config.deslop_cache);
        assert_eq!(config.deslop_cache_path, Some(PathBuf::from("/tmp/blocks")));
    }

    #[test]
    fn parses_skip_slopignore_for_full_statement() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("config.yaml");
        fs::write(&path, "skip_slopignore_for_full_statement: true\n").expect("write config");

        let config = load_config_from(&path).expect("should parse");
        assert!(config.skip_slopignore_for_full_statement);
    }

    #[test]
    fn default_config_yaml_is_parseable() {
        let yaml = default_config_yaml();
        let config: Config = serde_yaml::from_str(&yaml).expect("should parse");
        assert!(!config.connect_with_downloads_watcher);
        assert!(!config.auto_deslop);
        assert_eq!(config.graph_map_tokens, 2048);
        assert_eq!(config.graph_format, "repomap");
    }

    #[test]
    fn ensure_config_dir_creates_dir_and_default_file() {
        let temp = tempdir().expect("tempdir");
        let home = temp.path().to_path_buf();
        // SAFETY: this test runs single-threaded; mutating HOME is safe.
        unsafe {
            std::env::set_var("HOME", &home);
        }

        let config_path = ensure_config_dir().expect("should succeed");
        assert!(config_path.exists());
        assert!(config_path.is_file());

        let contents = fs::read_to_string(&config_path).expect("should read");
        assert!(contents.contains("connect_with_downloads_watcher:"));

        // second call should not overwrite
        fs::write(&config_path, "modified: true\n").expect("should write");
        ensure_config_dir().expect("should succeed again");
        let contents = fs::read_to_string(&config_path).expect("should read");
        assert_eq!(contents, "modified: true\n");

        // SAFETY: this test runs single-threaded.
        unsafe {
            std::env::set_var("HOME", "/tmp");
        }
    }
}
