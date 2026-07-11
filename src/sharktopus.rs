use std::path::PathBuf;
use std::process::Command;

use crate::config::Config;
use crate::error::SlopError;
use crate::pathing::expand_tilde;

const SLOP_PATTERN: &str = "*.slop.md";

const TAGGED_RULE_NAME: &str = "auto-unslop-tagged";
const ALL_RULE_NAME: &str = "auto-unslop-all";
const TAG_PREFIX: &str = "#SLOP_AUTO_UNslop";

const CLAUDE_SLOP_RULE: &str = "claude-slop-to-deslop";
const CLAUDE_OTHER_RULE: &str = "claude-other-to-output";
const CLAUDE_SLOP_RULE_DL: &str = "claude-slop-to-deslop-downloads";
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

pub fn ensure_rules(config: &Config) -> Result<Vec<String>, SlopError> {
    if !is_available() {
        return Err(SlopError::ConfigError(
            "sharktopus is not available on PATH".to_string(),
        ));
    }

    let mut messages = Vec::new();
    let existing_rules = list_rules()?;

    let to_deslop = config
        .to_deslop_folder
        .as_deref()
        .map(expand_tilde)
        .or_else(crate::config::default_to_deslop_folder)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("~"));
            home.join(".slop").join("to_deslop")
        });
    let to_deslop_str = to_deslop.to_string_lossy().to_string();

    // 1. Always ensure the tagged rule exists (fires on #SLOP_AUTO_UNslop files)
    if !has_rule_named(&existing_rules, TAGGED_RULE_NAME) {
        add_rule_tagged(&to_deslop_str)?;
        messages.push(format!("added Sharktopus rule '{}'", TAGGED_RULE_NAME));
    }

    // 2. Toggle the all rule based on config.auto_deslop
    if config.auto_deslop {
        if !has_rule_named(&existing_rules, ALL_RULE_NAME) {
            add_rule_all(&to_deslop_str)?;
            messages.push(format!("added Sharktopus rule '{}'", ALL_RULE_NAME));
        }
    } else {
        if has_rule_named(&existing_rules, ALL_RULE_NAME) {
            remove_rule(ALL_RULE_NAME)?;
            messages.push(format!("removed Sharktopus rule '{}' (auto_deslop is off)", ALL_RULE_NAME));
        }
    }

    // 3. Ensure Claude.ai routing rules exist
    for (name, in_dir, is_slop) in [
        (CLAUDE_SLOP_RULE, "~/Downloads/drive", true),
        (CLAUDE_OTHER_RULE, "~/Downloads/drive", false),
        (CLAUDE_SLOP_RULE_DL, "~/Downloads", true),
        (CLAUDE_OTHER_RULE_DL, "~/Downloads", false),
    ] {
        if !has_rule_named(&existing_rules, name) {
            if is_slop {
                add_claude_slop_rule(name, in_dir, &to_deslop_str)?;
            } else {
                add_claude_other_rule(name, in_dir)?;
            }
            messages.push(format!("added Sharktopus rule '{}'", name));
        }
    }

    Ok(messages)
}

fn list_rules() -> Result<String, SlopError> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let rules_path = home
        .as_ref()
        .map(|h| h.join(".config").join("sharktopus").join("rules.json"))
        .ok_or_else(|| {
            SlopError::ConfigError("HOME is not set".to_string())
        })?;

    let content = std::fs::read_to_string(&rules_path).map_err(|error| {
        SlopError::ConfigError(format!(
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

fn remove_rule(name: &str) -> Result<(), SlopError> {
    let output = Command::new("sharktopus")
        .args(["remove-rule", "--name", name])
        .output()
        .map_err(|error| {
            SlopError::ConfigError(format!("failed to run sharktopus remove-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("no matching rule") {
            return Err(SlopError::ConfigError(format!(
                "sharktopus remove-rule failed: {stderr}"
            )));
        }
    }

    Ok(())
}

fn add_rule_tagged(to_deslop: &str) -> Result<(), SlopError> {
    let output = Command::new("sharktopus")
        .args([
            "add-rule",
            "--name", TAGGED_RULE_NAME,
            "--seniority", "2",
            "--in-dir", to_deslop,
            "--glob", SLOP_PATTERN,
            "--first-line-prefix", TAG_PREFIX,
            "--run", "slop -d __FILE__",
        ])
        .output()
        .map_err(|error| {
            SlopError::ConfigError(format!("failed to run sharktopus add-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlopError::ConfigError(format!(
            "sharktopus add-rule (tagged) failed: {stderr}"
        )));
    }

    Ok(())
}

fn add_rule_all(to_deslop: &str) -> Result<(), SlopError> {
    let output = Command::new("sharktopus")
        .args([
            "add-rule",
            "--name", ALL_RULE_NAME,
            "--seniority", "1",
            "--in-dir", to_deslop,
            "--glob", SLOP_PATTERN,
            "--run", "slop -d __FILE__",
        ])
        .output()
        .map_err(|error| {
            SlopError::ConfigError(format!("failed to run sharktopus add-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlopError::ConfigError(format!(
            "sharktopus add-rule (all) failed: {stderr}"
        )));
    }

    Ok(())
}

fn add_claude_slop_rule(name: &str, in_dir: &str, to_deslop: &str) -> Result<(), SlopError> {
    let output = Command::new("sharktopus")
        .args([
            "add-rule",
            "--name", name,
            "--seniority", "3",
            "--in-dir", in_dir,
            "--source-domain", CLAUDE_DOMAIN,
            "--glob", SLOP_PATTERN,
            "--move-to", to_deslop,
        ])
        .output()
        .map_err(|error| {
            SlopError::ConfigError(format!("failed to run sharktopus add-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlopError::ConfigError(format!(
            "sharktopus add-rule (claude-slop) failed: {stderr}"
        )));
    }

    Ok(())
}

fn add_claude_other_rule(name: &str, in_dir: &str) -> Result<(), SlopError> {
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
            SlopError::ConfigError(format!("failed to run sharktopus add-rule: {error}"))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SlopError::ConfigError(format!(
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
        let output = "21C4D15A   auto-unslop-tagged      2          dir=~/.slop/to_desou… run:slop -d __FILE__\n";
        assert!(has_rule_named(output, "auto-unslop-tagged"));
        assert!(!has_rule_named(output, "auto-unslop-all"));
    }

    #[test]
    fn has_rule_named_detects_all_rule() {
        let output = "FF04E000   auto-unslop-all         1          dir=~/.slop/to_desou… run:slop -d __FILE__\n";
        assert!(has_rule_named(output, "auto-unslop-all"));
        assert!(!has_rule_named(output, "auto-unslop-tagged"));
    }

    #[test]
    fn has_rule_named_handles_empty_output() {
        assert!(!has_rule_named("", "any rule"));
    }

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(TAGGED_RULE_NAME, "auto-unslop-tagged");
        assert_eq!(ALL_RULE_NAME, "auto-unslop-all");
        assert_eq!(SLOP_PATTERN, "*.slop.md");
        assert_eq!(TAG_PREFIX, "#SLOP_AUTO_UNslop");
    }
}
