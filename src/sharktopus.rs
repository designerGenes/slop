use std::path::PathBuf;
use std::process::Command;

use crate::config::Config;
use crate::error::SoupifyError;
use crate::pathing::expand_tilde;

const SOUP_PATTERN: &str = "*.soup.md";

const TAGGED_RULE_NAME: &str = "auto-unsoupify-tagged";
const ALL_RULE_NAME: &str = "auto-unsoupify-all";
const TAG_PREFIX: &str = "#SOUP_AUTO_UNSOUPIFY";

const CLAUDE_SOUP_RULE: &str = "claude-soup-to-desoupify";
const CLAUDE_OTHER_RULE: &str = "claude-other-to-output";
const CLAUDE_SOUP_RULE_DL: &str = "claude-soup-to-desoupify-downloads";
const CLAUDE_OTHER_RULE_DL: &str = "claude-other-to-output-downloads";

const CLAUDE_DOMAIN: &str = "claude.ai";
const CLAUDE_OUTPUT: &str = "~/dev/output/Claude";

pub fn is_available() -> bool {
    Command::new("sharktopus")
        .arg("--help")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn ensure_rules(config: &Config) -> Result<Vec<String>, SoupifyError> {
    if !is_available() {
        return Err(SoupifyError::ConfigError(
            "sharktopus is not available on PATH".to_string(),
        ));
    }

    let mut messages = Vec::new();
    let existing_rules = list_rules()?;

    let to_desoupify = config
        .to_desoupify_folder
        .as_deref()
        .map(expand_tilde)
        .or_else(crate::config::default_to_desoupify_folder)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("~"));
            home.join(".soupify").join("to_desoupify")
        });
    let to_desoupify_str = to_desoupify.to_string_lossy().to_string();

    // 1. Always ensure the tagged rule exists (fires on #SOUP_AUTO_UNSOUPIFY files)
    if !has_rule_named(&existing_rules, TAGGED_RULE_NAME) {
        add_rule_tagged(&to_desoupify_str)?;
        messages.push(format!("added Sharktopus rule '{}'", TAGGED_RULE_NAME));
    }

    // 2. Toggle the all rule based on config.auto_desoupify
    if config.auto_desoupify {
        if !has_rule_named(&existing_rules, ALL_RULE_NAME) {
            add_rule_all(&to_desoupify_str)?;
            messages.push(format!("added Sharktopus rule '{}'", ALL_RULE_NAME));
        }
    } else {
        if has_rule_named(&existing_rules, ALL_RULE_NAME) {
            remove_rule(ALL_RULE_NAME)?;
            messages.push(format!("removed Sharktopus rule '{}' (auto_desoupify is off)", ALL_RULE_NAME));
        }
    }

    // 3. Ensure Claude.ai routing rules exist
    for (name, in_dir, is_soup) in [
        (CLAUDE_SOUP_RULE, "~/Downloads/drive", true),
        (CLAUDE_OTHER_RULE, "~/Downloads/drive", false),
        (CLAUDE_SOUP_RULE_DL, "~/Downloads", true),
        (CLAUDE_OTHER_RULE_DL, "~/Downloads", false),
    ] {
        if !has_rule_named(&existing_rules, name) {
            if is_soup {
                add_claude_soup_rule(name, in_dir, &to_desoupify_str)?;
            } else {
                add_claude_other_rule(name, in_dir)?;
            }
            messages.push(format!("added Sharktopus rule '{}'", name));
        }
    }

    Ok(messages)
}

fn list_rules() -> Result<String, SoupifyError> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let rules_path = home
        .as_ref()
        .map(|h| h.join(".config").join("sharktopus").join("rules.json"))
        .ok_or_else(|| {
            SoupifyError::ConfigError("HOME is not set".to_string())
        })?;

    let content = std::fs::read_to_string(&rules_path).map_err(|error| {
        SoupifyError::ConfigError(format!(
            "failed to read {}: {}",
            rules_path.display(),
            error
        ))
    })?;

    // Extract rule names from the JSON. We only need names for existence
    // checks, so a simple substring search on the raw JSON is sufficient
    // and avoids a serde_json dependency on Sharktopus's schema.
    Ok(content)
}

fn has_rule_named(rules_output: &str, name: &str) -> bool {
    rules_output.contains(name)
}

fn remove_rule(name: &str) -> Result<(), SoupifyError> {
    let output = Command::new("sharktopus")
        .args(["remove-rule", "--name", name])
        .output()
        .map_err(|error| {
            SoupifyError::ConfigError(format!("failed to run sharktopus remove-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("no matching rule") {
            return Err(SoupifyError::ConfigError(format!(
                "sharktopus remove-rule failed: {stderr}"
            )));
        }
    }

    Ok(())
}

fn add_rule_tagged(to_desoupify: &str) -> Result<(), SoupifyError> {
    let output = Command::new("sharktopus")
        .args([
            "add-rule",
            "--name", TAGGED_RULE_NAME,
            "--seniority", "2",
            "--in-dir", to_desoupify,
            "--glob", SOUP_PATTERN,
            "--first-line-prefix", TAG_PREFIX,
            "--run", "soupify -d __FILE__",
        ])
        .output()
        .map_err(|error| {
            SoupifyError::ConfigError(format!("failed to run sharktopus add-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SoupifyError::ConfigError(format!(
            "sharktopus add-rule (tagged) failed: {stderr}"
        )));
    }

    Ok(())
}

fn add_rule_all(to_desoupify: &str) -> Result<(), SoupifyError> {
    let output = Command::new("sharktopus")
        .args([
            "add-rule",
            "--name", ALL_RULE_NAME,
            "--seniority", "1",
            "--in-dir", to_desoupify,
            "--glob", SOUP_PATTERN,
            "--run", "soupify -d __FILE__",
        ])
        .output()
        .map_err(|error| {
            SoupifyError::ConfigError(format!("failed to run sharktopus add-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SoupifyError::ConfigError(format!(
            "sharktopus add-rule (all) failed: {stderr}"
        )));
    }

    Ok(())
}

fn add_claude_soup_rule(name: &str, in_dir: &str, to_desoupify: &str) -> Result<(), SoupifyError> {
    let output = Command::new("sharktopus")
        .args([
            "add-rule",
            "--name", name,
            "--seniority", "3",
            "--in-dir", in_dir,
            "--source-domain", CLAUDE_DOMAIN,
            "--glob", SOUP_PATTERN,
            "--move-to", to_desoupify,
        ])
        .output()
        .map_err(|error| {
            SoupifyError::ConfigError(format!("failed to run sharktopus add-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SoupifyError::ConfigError(format!(
            "sharktopus add-rule (claude-soup) failed: {stderr}"
        )));
    }

    Ok(())
}

fn add_claude_other_rule(name: &str, in_dir: &str) -> Result<(), SoupifyError> {
    let output = Command::new("sharktopus")
        .args([
            "add-rule",
            "--name", name,
            "--seniority", "1",
            "--in-dir", in_dir,
            "--source-domain", CLAUDE_DOMAIN,
            "--move-to", CLAUDE_OUTPUT,
        ])
        .output()
        .map_err(|error| {
            SoupifyError::ConfigError(format!("failed to run sharktopus add-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SoupifyError::ConfigError(format!(
            "sharktopus add-rule (claude-other) failed: {stderr}"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_rule_named_detects_existing_rule() {
        let output = "21C4D15A   auto-unsoupify-tagged      2          dir=~/.soupify/to_desou… run:soupify -d __FILE__\n";
        assert!(has_rule_named(output, "auto-unsoupify-tagged"));
        assert!(!has_rule_named(output, "auto-unsoupify-all"));
    }

    #[test]
    fn has_rule_named_detects_all_rule() {
        let output = "FF04E000   auto-unsoupify-all         1          dir=~/.soupify/to_desou… run:soupify -d __FILE__\n";
        assert!(has_rule_named(output, "auto-unsoupify-all"));
        assert!(!has_rule_named(output, "auto-unsoupify-tagged"));
    }

    #[test]
    fn has_rule_named_handles_empty_output() {
        assert!(!has_rule_named("", "any rule"));
    }

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(TAGGED_RULE_NAME, "auto-unsoupify-tagged");
        assert_eq!(ALL_RULE_NAME, "auto-unsoupify-all");
        assert_eq!(SOUP_PATTERN, "*.soup.md");
        assert_eq!(TAG_PREFIX, "#SOUP_AUTO_UNSOUPIFY");
    }
}
