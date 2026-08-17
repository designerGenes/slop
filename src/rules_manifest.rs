//! Executable DSL for all path-selection actions performed by `slop`.
//!
//! The `.slopignore` file supplies familiar gitignore-style path patterns. This
//! manifest determines how the resulting sources compose with command-line
//! requests and safety filters. It is deliberately a small text DSL rather
//! than a data serialization format so comments and precedence stay readable.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::OnceLock;

const MANIFEST: &str = include_str!("../resources/slop-rules.manifest");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Unlimited,
    CurrentDirectoryShallow,
    DirectFiles,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub depth: Option<Depth>,
}

#[derive(Debug, Clone, Copy)]
pub enum PathAction {
    Symlink,
    HardDirectory,
    UnsupportedFile,
    IgnoredExtension,
    GeneratedSlop,
    SlopIgnoreFile,
    NonText,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RuleId {
    CliExclude,
    DirectFile,
    SlopInclude,
    SlopIgnore,
    GitIgnore,
    DirectoryWalk,
    Symlink,
    HardDirectory,
    UnsupportedFile,
    IgnoredExtension,
    GeneratedSlop,
    SlopIgnoreFile,
    NonText,
    Duplicate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Include,
    Exclude,
    Skip,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Copy)]
struct Mode {
    input: InputKind,
    recursive: Option<bool>,
    current_directory: Option<bool>,
    depth: Option<Depth>,
}

#[derive(Debug, Clone, Copy)]
struct Rule {
    priority: u8,
    action: Action,
    condition: Option<RuleCondition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleCondition {
    RespectGitignore,
}

#[derive(Debug)]
struct RulesManifest {
    modes: Vec<Mode>,
    rules: HashMap<RuleId, Rule>,
}

fn manifest() -> &'static RulesManifest {
    static PARSED: OnceLock<RulesManifest> = OnceLock::new();
    PARSED.get_or_init(|| {
        parse_manifest(MANIFEST).expect("embedded slop rules manifest must be valid")
    })
}

fn parse_manifest(contents: &str) -> Result<RulesManifest, String> {
    let mut modes = Vec::new();
    let mut rules = HashMap::new();
    let mut mode_names = BTreeSet::new();

    for (line_number, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let fields: Vec<_> = line.split_whitespace().collect();
        match fields.first().copied() {
            Some("mode") if fields.len() == 6 => {
                let name = fields[1];
                if !mode_names.insert(name) {
                    return Err(format!("line {}: duplicate mode {name}", line_number + 1));
                }
                modes.push(Mode {
                    input: parse_value(fields[2], "input", line_number + 1, parse_input_kind)?,
                    recursive: parse_value(
                        fields[3],
                        "recursive",
                        line_number + 1,
                        parse_bool_or_any,
                    )?,
                    current_directory: parse_value(
                        fields[4],
                        "current",
                        line_number + 1,
                        parse_bool_or_any,
                    )?,
                    depth: parse_value(fields[5], "depth", line_number + 1, parse_depth)?,
                });
            }
            Some("rule") if (4..=5).contains(&fields.len()) => {
                let id = parse_rule_id(fields[1])?;
                if rules.contains_key(&id) {
                    return Err(format!(
                        "line {}: duplicate rule {}",
                        line_number + 1,
                        fields[1]
                    ));
                }
                let condition = if let Some(field) = fields.get(4) {
                    Some(parse_value(
                        field,
                        "when",
                        line_number + 1,
                        parse_rule_condition,
                    )?)
                } else {
                    None
                };
                if condition.is_some() && id != RuleId::GitIgnore {
                    return Err(format!(
                        "line {}: only gitignore may use a condition",
                        line_number + 1
                    ));
                }
                rules.insert(
                    id,
                    Rule {
                        priority: parse_value(fields[2], "priority", line_number + 1, |value| {
                            value
                                .parse::<u8>()
                                .map_err(|_| "expected 0..255".to_string())
                        })?,
                        action: parse_value(fields[3], "action", line_number + 1, parse_action)?,
                        condition,
                    },
                );
            }
            _ => {
                return Err(format!(
                    "line {}: invalid manifest statement",
                    line_number + 1
                ))
            }
        }
    }

    if modes.is_empty() {
        return Err("manifest defines no modes".to_string());
    }
    for required in RuleId::ALL {
        if !rules.contains_key(&required) {
            return Err(format!("manifest is missing rule {}", required.name()));
        }
    }
    Ok(RulesManifest { modes, rules })
}

fn parse_value<T>(
    field: &str,
    key: &str,
    line_number: usize,
    parse: impl FnOnce(&str) -> Result<T, String>,
) -> Result<T, String> {
    let Some(value) = field.strip_prefix(&format!("{key}=")) else {
        return Err(format!("line {line_number}: expected {key}=..."));
    };
    parse(value).map_err(|error| format!("line {line_number}: {key}: {error}"))
}

fn parse_input_kind(value: &str) -> Result<InputKind, String> {
    match value {
        "file" => Ok(InputKind::File),
        "directory" => Ok(InputKind::Directory),
        _ => Err("expected file or directory".to_string()),
    }
}

fn parse_bool_or_any(value: &str) -> Result<Option<bool>, String> {
    match value {
        "true" => Ok(Some(true)),
        "false" => Ok(Some(false)),
        "any" => Ok(None),
        _ => Err("expected true, false, or any".to_string()),
    }
}

fn parse_depth(value: &str) -> Result<Option<Depth>, String> {
    match value {
        "none" => Ok(None),
        "unlimited" => Ok(Some(Depth::Unlimited)),
        "current-directory-shallow" => Ok(Some(Depth::CurrentDirectoryShallow)),
        "direct-files" => Ok(Some(Depth::DirectFiles)),
        _ => Err("expected a known traversal depth".to_string()),
    }
}

fn parse_action(value: &str) -> Result<Action, String> {
    match value {
        "include" => Ok(Action::Include),
        "exclude" => Ok(Action::Exclude),
        "skip" => Ok(Action::Skip),
        "error" => Ok(Action::Error),
        _ => Err("expected include, exclude, skip, or error".to_string()),
    }
}

fn parse_rule_condition(value: &str) -> Result<RuleCondition, String> {
    match value {
        "respect-gitignore" => Ok(RuleCondition::RespectGitignore),
        _ => Err("expected respect-gitignore".to_string()),
    }
}

impl RuleId {
    const ALL: [Self; 14] = [
        Self::CliExclude,
        Self::DirectFile,
        Self::SlopInclude,
        Self::SlopIgnore,
        Self::GitIgnore,
        Self::DirectoryWalk,
        Self::Symlink,
        Self::HardDirectory,
        Self::UnsupportedFile,
        Self::IgnoredExtension,
        Self::GeneratedSlop,
        Self::SlopIgnoreFile,
        Self::NonText,
        Self::Duplicate,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::CliExclude => "cli-exclude",
            Self::DirectFile => "direct-file",
            Self::SlopInclude => "slopinclude",
            Self::SlopIgnore => "slopignore",
            Self::GitIgnore => "gitignore",
            Self::DirectoryWalk => "directory-walk",
            Self::Symlink => "symlink",
            Self::HardDirectory => "hard-directory",
            Self::UnsupportedFile => "unsupported-file",
            Self::IgnoredExtension => "ignored-extension",
            Self::GeneratedSlop => "generated-slop",
            Self::SlopIgnoreFile => "slopignore-file",
            Self::NonText => "non-text",
            Self::Duplicate => "duplicate",
        }
    }
}

fn parse_rule_id(value: &str) -> Result<RuleId, String> {
    RuleId::ALL
        .into_iter()
        .find(|id| id.name() == value)
        .ok_or_else(|| format!("unknown rule {value}"))
}

/// Return whether the highest-priority matching action includes the path.
fn selects(sources: &[RuleId]) -> bool {
    let rule = sources
        .iter()
        .filter_map(|source| manifest().rules.get(source))
        .max_by_key(|rule| rule.priority)
        .expect("selection must provide a manifest rule");
    rule.action == Action::Include
}

/// Resolve the manifest mode for a positional input.
pub fn policy_for(input: &Path, recursive: bool) -> Policy {
    let input_kind = if input.is_file() {
        InputKind::File
    } else {
        InputKind::Directory
    };
    let current_directory = is_current_dir(input);

    let mode = manifest()
        .modes
        .iter()
        .find(|mode| {
            mode.input == input_kind
                && mode.recursive.is_none_or(|value| value == recursive)
                && mode
                    .current_directory
                    .is_none_or(|value| value == current_directory)
        })
        .unwrap_or_else(|| panic!("slop rules manifest has no matching mode"));

    Policy { depth: mode.depth }
}

pub fn selects_direct_file(matches_cli_exclude: bool) -> bool {
    let sources = if matches_cli_exclude {
        [RuleId::DirectFile, RuleId::CliExclude]
    } else {
        [RuleId::DirectFile, RuleId::DirectFile]
    };
    selects(&sources)
}

pub fn selects_directory_file(matches_cli_exclude: bool) -> bool {
    let sources = if matches_cli_exclude {
        [RuleId::DirectoryWalk, RuleId::CliExclude]
    } else {
        [RuleId::DirectoryWalk, RuleId::DirectoryWalk]
    };
    selects(&sources)
}

/// Includes are evaluated separately so they can rescue paths pruned by
/// `.slopignore`/`.gitignore`; command-line excludes still win by priority.
pub fn selects_slopinclude(matches_cli_exclude: bool) -> bool {
    let sources = if matches_cli_exclude {
        [RuleId::SlopInclude, RuleId::CliExclude]
    } else {
        [RuleId::SlopInclude, RuleId::SlopInclude]
    };
    selects(&sources)
}

pub fn slopignore_excludes() -> bool {
    manifest().rules[&RuleId::SlopIgnore].action == Action::Exclude
}

pub fn runs_slopincludes() -> bool {
    manifest().rules[&RuleId::SlopInclude].action == Action::Include
}

pub fn gitignore_excludes(respect_gitignore: bool) -> bool {
    let rule = manifest().rules[&RuleId::GitIgnore];
    rule.action == Action::Exclude
        && match rule.condition {
            Some(RuleCondition::RespectGitignore) => respect_gitignore,
            None => true,
        }
}

pub fn skips(action: PathAction) -> bool {
    manifest().rules[&rule_id_for(action)].action == Action::Skip
}

pub fn errors(action: PathAction) -> bool {
    manifest().rules[&rule_id_for(action)].action == Action::Error
}

fn rule_id_for(action: PathAction) -> RuleId {
    match action {
        PathAction::Symlink => RuleId::Symlink,
        PathAction::HardDirectory => RuleId::HardDirectory,
        PathAction::UnsupportedFile => RuleId::UnsupportedFile,
        PathAction::IgnoredExtension => RuleId::IgnoredExtension,
        PathAction::GeneratedSlop => RuleId::GeneratedSlop,
        PathAction::SlopIgnoreFile => RuleId::SlopIgnoreFile,
        PathAction::NonText => RuleId::NonText,
        PathAction::Duplicate => RuleId::Duplicate,
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
    fn manifest_makes_cli_excludes_the_final_override() {
        assert!(selects_direct_file(false));
        assert!(!selects_direct_file(true));
        assert!(selects_slopinclude(false));
        assert!(!selects_slopinclude(true));
    }

    #[test]
    fn gitignore_rule_requires_explicit_opt_in() {
        assert!(!gitignore_excludes(false));
        assert!(gitignore_excludes(true));
    }

    #[test]
    fn embedded_manifest_covers_direct_and_atmospheric_selection() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        let file = root.join("file.rs");
        let directory = root.join("directory");
        fs::write(&file, "fn main() {}\n").expect("file");
        fs::create_dir(&directory).expect("directory");

        assert_eq!(policy_for(&file, false).depth, None);
        assert_eq!(
            policy_for(&directory, false).depth,
            Some(Depth::DirectFiles)
        );
        assert_eq!(policy_for(&directory, true).depth, Some(Depth::Unlimited));
    }

    #[test]
    fn manifest_rejects_unknown_or_missing_rules() {
        assert!(parse_manifest("rule unknown priority=1 action=include\n").is_err());
        assert!(
            parse_manifest("mode only input=file recursive=any current=any depth=none\n").is_err()
        );
    }
}
