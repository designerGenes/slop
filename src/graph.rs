use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::Config;
use crate::error::SlopError;
use crate::models::SoupMetaBlock;
use crate::repomap;

pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let trimmed = root.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

pub fn shared_git_root(files: &[PathBuf]) -> Option<PathBuf> {
    if files.is_empty() {
        return None;
    }
    let first_dir = files[0].parent()?;
    let root = find_git_root(first_dir)?;
    for file in files.iter().skip(1) {
        let Some(dir) = file.parent() else {
            return None;
        };
        let Some(other_root) = find_git_root(dir) else {
            return None;
        };
        if root != other_root {
            return None;
        }
    }
    Some(root)
}

pub fn generate_repomap(
    repo_root: &Path,
    seed_files: &[PathBuf],
    config: &Config,
) -> Result<SoupMetaBlock, SlopError> {
    let map_tokens = config.graph_map_tokens;

    let body = repomap::generate_repomap(repo_root, seed_files, map_tokens)
        .ok_or_else(|| {
            SlopError::RepoMapGenerationFailure(
                "no repository map could be generated".to_string(),
            )
        })?;

    let content_lines: Vec<String> = if body.is_empty() {
        Vec::new()
    } else {
        body.split('\n').map(ToString::to_string).collect()
    };

    let line_count = content_lines.len();

    Ok(SoupMetaBlock {
        label: "repo-graph".to_string(),
        kind: "codegraph".to_string(),
        format: config.graph_format.clone(),
        line_count,
        readonly: true,
        content_lines,
    })
}

pub fn should_include_graph(args_include_graph: bool, config: &Config) -> bool {
    args_include_graph || config.include_graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn finds_git_root_for_current_repo() {
        let cwd = std::env::current_dir().expect("cwd");
        if let Some(root) = find_git_root(&cwd) {
            assert!(root.join(".git").exists() || root.exists());
        }
    }

    #[test]
    fn returns_none_for_non_git_directory() {
        let temp = tempdir().expect("tempdir");
        let result = find_git_root(temp.path());
        assert!(result.is_none());
    }

    #[test]
    fn shared_git_root_returns_none_for_empty_files() {
        assert!(shared_git_root(&[]).is_none());
    }

    #[test]
    fn shared_git_root_returns_none_for_non_git_files() {
        let temp = tempdir().expect("tempdir");
        let file = temp.path().join("file.txt");
        std::fs::write(&file, "hello").expect("write");
        assert!(shared_git_root(&[file]).is_none());
    }

    #[test]
    fn should_include_graph_respects_flag() {
        let config = Config::default();
        assert!(should_include_graph(true, &config));
        assert!(!should_include_graph(false, &config));
    }

    #[test]
    fn should_include_graph_respects_config() {
        let mut config = Config::default();
        config.include_graph = true;
        assert!(should_include_graph(false, &config));
    }
}
