use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::SoupifyError;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub connect_with_downloads_watcher: bool,
    pub auto_desoupify: bool,
    pub warn_before_overwriting: bool,
    pub to_desoupify_folder: Option<PathBuf>,
    pub soupified_folder: Option<PathBuf>,
    pub include_graph: bool,
    pub respect_gitignore: bool,
    pub graph_map_tokens: usize,
    pub graph_format: String,
    pub graph_force_include_supertypes: bool,
    pub index_dir: Option<PathBuf>,
    pub selection_default_hops: usize,
    pub top_k: usize,
    pub max_soup_bytes: usize,
    pub allow_fuzzy_task: bool,
    pub selection_provenance: bool,
    pub selection_provenance_max_bytes: usize,
    pub secret_scan: String,
    pub redact_secrets: bool,
    pub secret_rules_path: Option<PathBuf>,
    pub graph_token_model: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connect_with_downloads_watcher: false,
            auto_desoupify: false,
            warn_before_overwriting: false,
            to_desoupify_folder: None,
            soupified_folder: None,
            include_graph: false,
            respect_gitignore: false,
            graph_map_tokens: 2048,
            graph_format: "repomap".to_string(),
            graph_force_include_supertypes: true,
            index_dir: None,
            selection_default_hops: 1,
            top_k: 12,
            max_soup_bytes: 1_048_576,
            allow_fuzzy_task: true,
            selection_provenance: false,
            selection_provenance_max_bytes: 2048,
            secret_scan: "warn".to_string(),
            redact_secrets: false,
            secret_rules_path: None,
            graph_token_model: "o200k_base".to_string(),
        }
    }
}

pub fn load_config() -> Config {
    let Some(config_path) = default_config_path() else {
        return Config::default();
    };
    load_config_from(&config_path).unwrap_or_default()
}

pub fn load_config_from(path: &Path) -> Result<Config, SoupifyError> {
    let contents = fs::read_to_string(path).map_err(|error| {
        SoupifyError::ConfigError(format!("{}: {}", path.display(), error))
    })?;
    serde_yaml::from_str(&contents)
        .map_err(|error| SoupifyError::ConfigError(format!("{}: {}", path.display(), error)))
}

pub fn default_config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".config").join("soupify").join("config.yaml"))
}

pub fn default_config_yaml() -> String {
    format!(
        "# Soupify configuration\n\
         # Settings here are scanned at every invocation and can be overridden\n\
         # by command-line flags.\n\n\
         # Connect with Sharktopus (downloads watcher). If true, Soupify will\n\
         # add/verify a rule to automatically move .soup.md files downloaded to\n\
         # $HOME/Downloads into the \"to desoupify\" folder.\n\
         connect_with_downloads_watcher: {connect_watcher}\n\n\
         # If true, Soupify will automatically de-soupify any Soup files in the\n\
         # \"to desoupify\" folder. If false, they are only moved there and the\n\
         # user must manually run Soupify to de-soupify them.\n\
         auto_desoupify: {auto_desoupify}\n\n\
         # If true, Soupify will warn before overwriting existing files during\n\
         # de-soupification.\n\
         warn_before_overwriting: {warn_overwrite}\n\n\
         # Path to the folder where Soup files are moved for de-soupification.\n\
         # Defaults to $HOME/.soupify/to_desoupify\n\
         to_desoupify_folder: {to_desoupify}\n\n\
         # Path to the folder where Soupified files are saved. Defaults to\n\
         # $HOME/.soupify/soupified. Can be overridden at invocation with\n\
         # --soupify-to.\n\
         soupified_folder: {soupified}\n\n\
         # Include a code-graph metadata block when soupifying. Override per-run\n\
         # with --include-graph.\n\
         include_graph: {include_graph}\n\n\
         # If true, Soupify will skip any files or folders matched by the\n\
         # target repo's .gitignore when walking a directory. Override\n\
         # per-run with --respect-gitignore.\n\
         respect_gitignore: {respect_gitignore}\n\n\
         # RepoMapper --map-tokens; compactness lever for the graph.\n\
         graph_map_tokens: {graph_tokens}\n\n\
         # Graph format: repomap | dot | json | mermaid\n\
         graph_format: {graph_format}\n\n\
         # Force-include declared protocols/superclasses of seed files.\n\
         graph_force_include_supertypes: {force_supertypes}\n\n\
         # Directory for the selection full-text index. Defaults to\n\
         # $HOME/.cache/soupify/index. Lives outside any repo tree.\n\
         index_dir: {index_dir}\n\n\
         # Default BFS radius around --seed files.\n\
         selection_default_hops: {sel_hops}\n\n\
         # Max files selected by --match/--task/--symbol/--seed.\n\
         top_k: {top_k}\n\n\
         # Hard ceiling on serialized soup bytes (1 MiB default).\n\
         max_soup_bytes: {max_soup_bytes}\n\n\
         # If false, --task is rejected (deterministic-only mode).\n\
         allow_fuzzy_task: {allow_fuzzy}\n\n\
         # Emit a selection provenance #SOUP_META block.\n\
         selection_provenance: {sel_prov}\n\n\
         # Max bytes for the provenance block.\n\
         selection_provenance_max_bytes: {sel_prov_max}\n\n\
         # Secret scan mode: off | warn | block. 'off' disables all scanning;\n\
         # 'warn' (default) prints warnings but never blocks; 'block' refuses\n\
         # soups containing high-confidence secret patterns (private keys,\n\
         # AWS/Google/Stripe tokens, JWTs, etc.). Override per-run with\n\
         # --allow-secrets or --redact.\n\
         secret_scan: {secret_scan}\n",
        connect_watcher = false,
        auto_desoupify = false,
        warn_overwrite = false,
        to_desoupify = "~/.soupify/to_desoupify",
        soupified = "~/.soupify/soupified",
        include_graph = false,
        respect_gitignore = false,
        graph_tokens = 2048,
        graph_format = "repomap",
        force_supertypes = true,
        index_dir = "~/.cache/soupify/index",
        sel_hops = 1,
        top_k = 12,
        max_soup_bytes = 1_048_576,
        allow_fuzzy = true,
        sel_prov = false,
        sel_prov_max = 2048,
        secret_scan = "warn",
    )
}

pub fn ensure_config_dir() -> Result<PathBuf, SoupifyError> {
    let config_path =
        default_config_path().ok_or(SoupifyError::HomeDirectoryResolutionFailure)?;
    let config_dir = config_path
        .parent()
        .ok_or_else(|| SoupifyError::ConfigError("config path has no parent directory".to_string()))?;

    fs::create_dir_all(config_dir).map_err(|error| SoupifyError::DirectoryCreationFailure {
        path: config_dir.to_path_buf(),
        source: error,
    })?;

    if !config_path.exists() {
        fs::write(&config_path, default_config_yaml()).map_err(|error| {
            SoupifyError::FileWriteFailure {
                path: config_path.clone(),
                source: error,
            }
        })?;
    }

    Ok(config_path)
}

pub fn default_to_desoupify_folder() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".soupify").join("to_desoupify"))
}

pub fn default_soupified_folder() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".soupify").join("soupified"))
}

pub fn default_index_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(home.join(".cache").join("soupify").join("index"))
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
        fs::write(&path, "auto_desoupify: true\ngraph_map_tokens: 4096\n")
            .expect("write config");

        let config = load_config_from(&path).expect("should parse");
        assert!(config.auto_desoupify);
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
             auto_desoupify: true\n\
             warn_before_overwriting: true\n\
             to_desoupify_folder: /tmp/to_desoupify\n\
             soupified_folder: /tmp/soupified\n\
             include_graph: true\n\
             respect_gitignore: true\n\
             graph_map_tokens: 1024\n\
             graph_format: dot\n\
             graph_force_include_supertypes: false\n",
        )
        .expect("write config");

        let config = load_config_from(&path).expect("should parse");
        assert!(config.connect_with_downloads_watcher);
        assert!(config.auto_desoupify);
        assert!(config.warn_before_overwriting);
        assert_eq!(
            config.to_desoupify_folder,
            Some(PathBuf::from("/tmp/to_desoupify"))
        );
        assert_eq!(
            config.soupified_folder,
            Some(PathBuf::from("/tmp/soupified"))
        );
        assert!(config.include_graph);
        assert!(config.respect_gitignore);
        assert_eq!(config.graph_map_tokens, 1024);
        assert_eq!(config.graph_format, "dot");
        assert!(!config.graph_force_include_supertypes);
    }

    #[test]
    fn default_config_has_expected_values() {
        let config = Config::default();
        assert!(!config.connect_with_downloads_watcher);
        assert!(!config.auto_desoupify);
        assert!(!config.warn_before_overwriting);
        assert!(config.to_desoupify_folder.is_none());
        assert!(config.soupified_folder.is_none());
        assert!(!config.include_graph);
        assert!(!config.respect_gitignore);
        assert_eq!(config.graph_map_tokens, 2048);
        assert_eq!(config.graph_format, "repomap");
        assert!(config.graph_force_include_supertypes);
    }

    #[test]
    fn default_config_yaml_contains_all_keys() {
        let yaml = default_config_yaml();
        assert!(yaml.contains("connect_with_downloads_watcher:"));
        assert!(yaml.contains("auto_desoupify:"));
        assert!(yaml.contains("warn_before_overwriting:"));
        assert!(yaml.contains("to_desoupify_folder:"));
        assert!(yaml.contains("soupified_folder:"));
        assert!(yaml.contains("include_graph:"));
        assert!(yaml.contains("respect_gitignore:"));
        assert!(yaml.contains("graph_map_tokens:"));
        assert!(yaml.contains("graph_format:"));
        assert!(yaml.contains("graph_force_include_supertypes:"));
        assert!(yaml.contains("secret_scan:"));
    }

    #[test]
    fn default_config_yaml_is_parseable() {
        let yaml = default_config_yaml();
        let config: Config = serde_yaml::from_str(&yaml).expect("should parse");
        assert!(!config.connect_with_downloads_watcher);
        assert!(!config.auto_desoupify);
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
