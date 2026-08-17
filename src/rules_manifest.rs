//! The embedded DSL that defines slop's path-selection policy.
//!
//! `.slopignore` remains the user-facing, gitignore-compatible pattern
//! language. This manifest defines when that language applies and how it
//! composes with direct paths, traversal depth, and include directives.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

const MANIFEST: &str = include_str!("../resources/slop-rules.yaml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Unlimited,
    CurrentDirectoryShallow,
    DirectFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub depth: Option<Depth>,
    pub apply_excludes: bool,
    pub apply_ignore_patterns: bool,
    pub run_include_directives: bool,
    pub include_precedence: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesManifest {
    version: u8,
    rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rule {
    id: String,
    when: Condition,
    then: Actions,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Condition {
    input: InputKind,
    #[serde(default)]
    recursive: Option<bool>,
    #[serde(default)]
    current_directory: Option<bool>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum InputKind {
    File,
    Directory,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Actions {
    #[serde(default)]
    depth: Option<Depth>,
    apply_excludes: bool,
    apply_ignore_patterns: bool,
    run_include_directives: bool,
    #[serde(default)]
    include_precedence: Option<IncludePrecedence>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum IncludePrecedence {
    Force,
}

impl<'de> Deserialize<'de> for Depth {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        match value.as_str() {
            "unlimited" => Ok(Self::Unlimited),
            "current-directory-shallow" => Ok(Self::CurrentDirectoryShallow),
            "direct-files" => Ok(Self::DirectFiles),
            _ => Err(serde::de::Error::unknown_variant(
                &value,
                &["unlimited", "current-directory-shallow", "direct-files"],
            )),
        }
    }
}

fn manifest() -> &'static RulesManifest {
    static PARSED: OnceLock<RulesManifest> = OnceLock::new();
    PARSED.get_or_init(|| {
        let manifest: RulesManifest =
            serde_yaml::from_str(MANIFEST).expect("embedded slop rules manifest must be valid");
        assert_eq!(
            manifest.version, 1,
            "unsupported slop rules manifest version"
        );
        assert!(
            !manifest.rules.is_empty(),
            "embedded slop rules manifest must define rules"
        );
        let mut rule_ids = BTreeSet::new();
        for rule in &manifest.rules {
            assert!(!rule.id.trim().is_empty(), "slop rules must have an id");
            assert!(
                rule_ids.insert(&rule.id),
                "slop rules manifest has duplicate rule id: {}",
                rule.id
            );
        }
        manifest
    })
}

/// Resolve the first manifest rule matching a positional input. The boolean
/// `recursive` is the CLI's `-r` setting; callers with a custom finite depth
/// retain that depth unless this policy is used for normal CLI traversal.
pub fn policy_for(input: &Path, recursive: bool) -> Policy {
    let input_kind = if input.is_file() {
        InputKind::File
    } else {
        InputKind::Directory
    };
    let current_directory = is_current_dir(input);

    let rule = manifest()
        .rules
        .iter()
        .find(|rule| {
            rule.when.input == input_kind
                && rule.when.recursive.is_none_or(|value| value == recursive)
                && rule
                    .when
                    .current_directory
                    .is_none_or(|value| value == current_directory)
        })
        .unwrap_or_else(|| panic!("slop rules manifest has no rule for {input_kind:?}"));

    Policy {
        depth: rule.then.depth,
        apply_excludes: rule.then.apply_excludes,
        apply_ignore_patterns: rule.then.apply_ignore_patterns,
        run_include_directives: rule.then.run_include_directives,
        include_precedence: rule.then.include_precedence == Some(IncludePrecedence::Force),
    }
}

fn is_current_dir(input: &Path) -> bool {
    let cwd = std::env::current_dir().ok();
    let resolved = std::fs::canonicalize(input).ok();
    cwd.as_ref()
        .and_then(|cwd| resolved.as_ref().map(|resolved| cwd == resolved))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn embedded_manifest_covers_direct_and_atmospheric_selection() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        let file = root.join("file.rs");
        let directory = root.join("directory");
        fs::write(&file, "fn main() {}\n").expect("file");
        fs::create_dir(&directory).expect("directory");

        let direct = policy_for(&file, false);
        assert!(!direct.apply_ignore_patterns);
        assert!(direct.run_include_directives);

        let named_directory = policy_for(&directory, false);
        assert_eq!(named_directory.depth, Some(Depth::DirectFiles));
        assert!(named_directory.apply_ignore_patterns);
        assert!(named_directory.include_precedence);

        let recursive = policy_for(&directory, true);
        assert_eq!(recursive.depth, Some(Depth::Unlimited));
    }

    #[test]
    fn manifest_rejects_unknown_fields() {
        let invalid = "version: 1\nrules: []\nunknown: true\n";
        assert!(serde_yaml::from_str::<RulesManifest>(invalid).is_err());
    }
}
