use std::collections::HashSet;

const IMPORTANT_FILENAMES: &[&str] = &[
    "README.md", "README.txt", "readme.md", "README.rst", "README",
    "requirements.txt", "Pipfile", "pyproject.toml", "setup.py", "setup.cfg",
    "package.json", "yarn.lock", "package-lock.json", "npm-shrinkwrap.json",
    "Dockerfile", "docker-compose.yml", "docker-compose.yaml",
    ".gitignore", ".gitattributes", ".dockerignore",
    "Makefile", "makefile", "CMakeLists.txt",
    "LICENSE", "LICENSE.txt", "LICENSE.md", "COPYING",
    "CHANGELOG.md", "CHANGELOG.txt", "HISTORY.md",
    "CONTRIBUTING.md", "CODE_OF_CONDUCT.md",
    ".env", ".env.example", ".env.local",
    "tox.ini", "pytest.ini", ".pytest.ini",
    ".flake8", ".pylintrc", "mypy.ini",
    "go.mod", "go.sum", "Cargo.toml", "Cargo.lock",
    "pom.xml", "build.gradle", "build.gradle.kts",
    "composer.json", "composer.lock",
    "Gemfile", "Gemfile.lock",
    // slop's own project conventions.
    ".slopignore", "slop.yaml", "slop.yml",
];

fn has_ext(file_name: &str, exts: &[&str]) -> bool {
    exts.iter().any(|ext| file_name.ends_with(ext))
}

pub fn is_important(rel_path: &str) -> bool {
    let normalized = std::path::Path::new(rel_path);
    let file_name = normalized
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let dir_name = normalized
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("");

    // `&&` binds tighter than `||`, so the previous spelling
    //     dir == ".github/workflows" && ends_with(".yml") || ends_with(".yaml")
    // parsed as (dir && .yml) || .yaml — every .yaml file anywhere in the tree
    // was classified important. The parentheses are the fix.
    if dir_name == ".github/workflows" && has_ext(file_name, &[".yml", ".yaml"]) {
        return true;
    }
    if dir_name == ".github" && has_ext(file_name, &[".md", ".yml", ".yaml"]) {
        return true;
    }
    if dir_name == "docs" && has_ext(file_name, &[".md", ".rst", ".txt"]) {
        return true;
    }

    IMPORTANT_FILENAMES.contains(&file_name)
}

pub fn filter_important_files(rel_paths: &[String]) -> HashSet<String> {
    rel_paths
        .iter()
        .filter(|p| is_important(p))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_files_are_important() {
        assert!(is_important(".github/workflows/ci.yml"));
        assert!(is_important(".github/workflows/release.yaml"));
    }

    #[test]
    fn stray_yaml_files_are_not_important() {
        // Regression: the precedence bug made every one of these return true.
        assert!(!is_important("src/random.yaml"));
        assert!(!is_important("deep/nested/dir/k8s.yaml"));
        assert!(!is_important("fixtures/testdata/sample.yaml"));
    }

    #[test]
    fn docker_compose_is_still_matched_by_name() {
        // Matched via IMPORTANT_FILENAMES, not the directory rules.
        assert!(is_important("docker-compose.yaml"));
        assert!(is_important("docker-compose.yml"));
    }

    #[test]
    fn github_and_docs_directories_are_scoped() {
        assert!(is_important(".github/PULL_REQUEST_TEMPLATE.md"));
        assert!(is_important("docs/architecture.md"));
        assert!(!is_important("src/docs_helper.md"));
        assert!(!is_important("elsewhere/notes.rst"));
    }

    #[test]
    fn build_manifests_are_important() {
        assert!(is_important("Cargo.toml"));
        assert!(is_important("README.md"));
        assert!(is_important(".slopignore"));
        assert!(!is_important("src/main.rs"));
    }

    #[test]
    fn filter_important_files_selects_the_expected_subset() {
        let paths: Vec<String> = ["Cargo.toml", "src/main.rs", "notes.yaml"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let kept = filter_important_files(&paths);
        assert_eq!(kept.len(), 1);
        assert!(kept.contains("Cargo.toml"));
    }
}
