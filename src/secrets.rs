use std::path::Path;

use crate::error::SlopError;
use crate::models::SourceFile;

#[derive(Debug, Clone)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub rule: String,
    pub severity: Severity,
    pub masked_value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Severity {
    Block,
    Warn,
}

struct Rule {
    name: &'static str,
    pattern: regex::Regex,
    severity: Severity,
}

fn build_rules() -> Vec<Rule> {
    vec![
        Rule {
            name: "private_key",
            pattern: regex::Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "aws_access_key_id",
            pattern: regex::Regex::new(r"AKIA[0-9A-Z]{16}").unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "google_api_key",
            pattern: regex::Regex::new(r"AIza[0-9A-Za-z_-]{35}").unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "twilio_account_sid",
            pattern: regex::Regex::new(r"AC[0-9a-f]{32}").unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "slack_token",
            pattern: regex::Regex::new(r"xox[bp]-[0-9a-zA-Z-]+").unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "github_token",
            pattern: regex::Regex::new(r"gh[ps]_[A-Za-z0-9]{36}").unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "stripe_secret_key",
            pattern: regex::Regex::new(r"sk_live_[0-9a-zA-Z]{24,}").unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "jwt",
            pattern: regex::Regex::new(r"eyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+").unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "bearer_token",
            pattern: regex::Regex::new(r#"(?i)bearer\s+[A-Za-z0-9._-]{20,}"#).unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "authorization_assignment",
            pattern: regex::Regex::new(r#"(?i)authorization\s*[:=]\s*['"]?[A-Za-z0-9._-]{20,}"#).unwrap(),
            severity: Severity::Block,
        },
        Rule {
            name: "dotenv_secret",
            pattern: regex::Regex::new(r#"^[A-Z0-9_]+\s*=\s*['"]?[A-Za-z0-9+/=._-]{20,}"#).unwrap(),
            severity: Severity::Block,
        },
    ]
}

fn is_sensitive_filename(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let lower = name.to_lowercase();
    lower.starts_with(".env")
        || lower.contains("secret")
        || lower.contains("credential")
        || lower == "id_rsa"
        || lower.ends_with(".pem")
        || lower.ends_with(".p12")
        || lower.ends_with(".keystore")
}

/// npm/Subresource-Integrity style content digests (`sha512-<base64>`, and
/// friends). These are checksums, never credentials, no matter which file
/// they appear in.
fn is_content_digest(token: &str) -> bool {
    const DIGEST_PREFIXES: &[&str] = &["sha1-", "sha256-", "sha384-", "sha512-", "sha512/", "md5-"];
    DIGEST_PREFIXES.iter().any(|prefix| token.starts_with(prefix))
}

/// Machine-generated dependency lockfiles consist almost entirely of registry
/// content digests that look like high-entropy secrets but never are. The
/// high-entropy heuristic is skipped for them; concrete pattern rules (private
/// keys, provider tokens, ...) still apply.
fn is_dependency_lockfile(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    matches!(
        name.to_lowercase().as_str(),
        "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lockb"
            | "bun.lock"
            | "cargo.lock"
            | "poetry.lock"
            | "pipfile.lock"
            | "uv.lock"
            | "gemfile.lock"
            | "composer.lock"
            | "go.sum"
            | "flake.lock"
            | "deno.lock"
    )
}

fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut freq = std::collections::HashMap::new();
    for c in s.chars() {
        *freq.entry(c).or_insert(0) += 1;
    }
    let len = s.len() as f64;
    let mut entropy = 0.0;
    for &count in freq.values() {
        let p = count as f64 / len;
        entropy -= p * p.log2();
    }
    entropy
}

fn is_secret_charset(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '_' || c == '-'
}

fn char_classes(s: &str) -> (bool, bool, bool) {
    let mut upper = false;
    let mut lower = false;
    let mut digit = false;
    for c in s.chars() {
        match c {
            'A'..='Z' => upper = true,
            'a'..='z' => lower = true,
            '0'..='9' => digit = true,
            _ => {}
        }
    }
    (upper, lower, digit)
}

fn looks_like_secret_token(token: &str) -> bool {
    if token.len() < 20 {
        return false;
    }
    if !token.chars().all(is_secret_charset) {
        return false;
    }
    let (upper, lower, digit) = char_classes(token);
    let classes = [upper, lower, digit].iter().filter(|&&f| f).count();
    classes >= 3 && shannon_entropy(token) > 4.5
}

fn mask_value(value: &str) -> String {
    if value.len() <= 8 {
        return "«REDACTED»".to_string();
    }
    format!("«REDACTED:{}...»", &value[..4])
}

fn is_suppressed(line: &str) -> bool {
    line.contains("# slop:allow-secret")
}

pub fn scan_files(files: &[SourceFile]) -> Vec<Finding> {
    let rules = build_rules();
    let mut findings = Vec::new();

    for file in files {
        let rel_name = file.file_name.clone();

        if is_sensitive_filename(Path::new(&file.original_absolute_path)) {
            findings.push(Finding {
                file: rel_name.clone(),
                line: 0,
                rule: "sensitive_filename".to_string(),
                severity: Severity::Block,
                masked_value: mask_value(&rel_name),
            });
        }

        let skip_entropy = is_dependency_lockfile(Path::new(&file.original_absolute_path));

        for (i, line) in file.contents.lines().enumerate() {
            if is_suppressed(line) {
                continue;
            }

            for rule in &rules {
                if let Some(m) = rule.pattern.find(line) {
                    findings.push(Finding {
                        file: rel_name.clone(),
                        line: i + 1,
                        rule: rule.name.to_string(),
                        severity: rule.severity.clone(),
                        masked_value: mask_value(m.as_str()),
                    });
                }
            }

            if skip_entropy {
                continue;
            }

            for token in line.split(|c: char| c.is_whitespace() || c == '=' || c == '"' || c == '\'') {
                if is_content_digest(token) {
                    continue;
                }
                if !looks_like_secret_token(token) {
                    continue;
                }
                let already_found = findings.iter().any(|f| {
                    f.line == i + 1 && f.file == rel_name
                });
                if !already_found {
                    findings.push(Finding {
                        file: rel_name.clone(),
                        line: i + 1,
                        rule: "high_entropy".to_string(),
                        severity: Severity::Warn,
                        masked_value: mask_value(token),
                    });
                }
            }
        }
    }

    findings
}

pub fn apply_redaction(files: &mut [SourceFile], findings: &[Finding]) {
    for file in files.iter_mut() {
        let mut lines: Vec<String> = file.contents.lines().map(String::from).collect();
        let mut changed = false;

        for finding in findings {
            if finding.file == file.file_name && finding.line > 0 && finding.line <= lines.len() {
                let line = &mut lines[finding.line - 1];
                for rule in build_rules() {
                    if rule.name == finding.rule {
                        if let Some(m) = rule.pattern.find(line) {
                            let replacement = format!("«REDACTED:{}»", finding.rule);
                            line.replace_range(m.range(), &replacement);
                            changed = true;
                        }
                    }
                }
            }
        }

        if changed {
            let trailing = file.contents.ends_with('\n');
            let mut redacted = lines.join("\n");
            if trailing {
                redacted.push('\n');
            }
            let (count, trailing) = crate::slop_format::analyze_contents(&redacted);
            file.contents = redacted;
            file.logical_line_count = count;
            file.trailing_newline = trailing;
            file.read_only = true;
            file.base_sha = None;
        }
    }
}

/// Render findings as one concise entry per (file, rule) group so a file with
/// dozens of hits produces one line instead of a wall of text. The first
/// occurrence's line number is kept; later ones collapse into a count.
pub fn findings_summary(findings: &[Finding]) -> String {
    let mut groups: Vec<(String, String, Severity, usize, usize, String)> = Vec::new();
    for finding in findings {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.0 == finding.file && group.1 == finding.rule)
        {
            group.3 += 1;
        } else {
            groups.push((
                finding.file.clone(),
                finding.rule.clone(),
                finding.severity.clone(),
                1,
                finding.line,
                finding.masked_value.clone(),
            ));
        }
    }

    groups
        .into_iter()
        .map(|(file, rule, severity, count, first_line, mask)| {
            let severity = if severity == Severity::Block {
                "BLOCK"
            } else {
                "WARN"
            };
            if count == 1 {
                format!("{file}:{first_line} {rule} [{severity}] {mask}")
            } else {
                format!("{file}: {rule} [{severity}] ×{count} (first at line {first_line}) {mask}")
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn enforce(
    files: &[SourceFile],
    config: &crate::config::Config,
    allow_secrets: bool,
    redact: bool,
) -> Result<Vec<SourceFile>, SlopError> {
    let mode = config.secret_scan.trim().to_lowercase();
    let disabled = mode == "off" || mode == "disabled" || mode == "false" || mode == "none";
    let block_mode = mode == "block" || mode == "strict";

    if disabled {
        return Ok(files.to_vec());
    }

    let findings = scan_files(files);

    if findings.is_empty() {
        return Ok(files.to_vec());
    }

    let has_block = findings.iter().any(|f| f.severity == Severity::Block);
    let _has_warn = findings.iter().any(|f| f.severity == Severity::Warn);

    let summary = findings_summary(&findings);

    if allow_secrets {
        eprintln!("warning: secrets detected but --allow-secrets bypasses: {}", summary);
        if redact {
            let mut files_mut = files.to_vec();
            apply_redaction(&mut files_mut, &findings);
            return Ok(files_mut);
        }
        return Ok(files.to_vec());
    }

    if redact {
        let mut files_mut = files.to_vec();
        apply_redaction(&mut files_mut, &findings);
        eprintln!("redacted {} findings in {} files", findings.len(), files.len());
        return Ok(files_mut);
    }

    if has_block && block_mode {
        return Err(SlopError::SecretsDetected {
            findings_summary: summary,
        });
    }

    eprintln!("warning: potential secrets detected: {}", summary);

    Ok(files.to_vec())
}

#[cfg(test)]
mod tests {
    use super::{looks_like_secret_token, scan_files, Severity};
    use crate::models::SourceFile;
    use std::path::PathBuf;

    fn source_file(name: &str, contents: &str) -> SourceFile {
        SourceFile {
            original_absolute_path: PathBuf::from(name),
            file_name: name.to_string(),
            name_token: name.to_string(),
            contents: contents.to_string(),
            logical_line_count: 1,
            trailing_newline: false,
            base_sha: None,
            read_only: false,
        }
    }

    fn high_entropy_findings(files: &[SourceFile]) -> Vec<(String, usize)> {
        scan_files(files)
            .into_iter()
            .filter(|f| f.rule == "high_entropy")
            .map(|f| (f.file, f.line))
            .collect()
    }

    #[test]
    fn flags_opaque_mixed_class_secret_like_tokens() {
        assert!(looks_like_secret_token(
            "aB3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHi"
        ));
        assert!(looks_like_secret_token(
            "7B3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHiJkLm"
        ));
    }

    #[test]
    fn rejects_source_code_tokens() {
        assert!(!looks_like_secret_token("fs::read_to_string(path)"));
        assert!(!looks_like_secret_token("Vec::new()"));
        assert!(!looks_like_secret_token("assert_eq!(a, b)"));
        assert!(!looks_like_secret_token("parser.c"));
        assert!(!looks_like_secret_token("registry+https://github.com"));
    }

    #[test]
    fn rejects_lowercase_hex_checksums() {
        assert!(!looks_like_secret_token(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
    }

    #[test]
    fn rejects_short_or_single_class_tokens() {
        assert!(!looks_like_secret_token("short"));
        assert!(!looks_like_secret_token("onlylowercaselettershere"));
        assert!(!looks_like_secret_token("ONLYUPPERCASELETTERSHERE"));
    }

    #[test]
    fn scan_files_skips_code_lines_but_flags_secret_like_token() {
        let src = "use std::fs;\n\
                   let api = \"aB3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHi\";\n";
        let findings = high_entropy_findings(&[source_file("main.rs", src)]);

        assert_eq!(findings, vec![("main.rs".to_string(), 2)]);
    }

    #[test]
    fn scan_files_does_not_flag_cargo_lock_checksums() {
        let src = "name = \"regex\"\n\
                   version = \"1.12.2\"\n\
                   source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
                   checksum = \"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\"\n";
        let findings = high_entropy_findings(&[source_file("Cargo.lock", src)]);

        assert!(findings.is_empty(), "findings: {:?}", findings);
    }

    #[test]
    fn scan_files_does_not_flag_package_lock_integrity_hashes() {
        let src = "{\n\
                   \t\"integrity\": \"sha512-aB3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHiJkLmNoPqRsTuVwXyZ0aBcDeFgHiJkLmNoPq\",\n\
                   \t\"integrity\": \"sha512-kJ9mN2pQr7sT4vUwXyZ0aBcDeFgHiJkLmNoPqRsTuVwXyZ0aBcDeFgHiJkLmNoPqAb3\",\n\
                   \t\"resolved\": \"https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz\"\n\
                   }\n";
        let findings = high_entropy_findings(&[source_file("package-lock.json", src)]);

        assert!(findings.is_empty(), "findings: {:?}", findings);
    }

    #[test]
    fn high_entropy_skipped_for_lockfiles_but_pattern_rules_still_apply() {
        let src = "yarn lockfile v1\n\
                   checksum aB3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHiJkLmNoPqRsTuVwXyZ0aBcDeFg\n\
                   AWS_ACCESS_KEY_ID=AKIA1234567890123456\n";
        let findings = scan_files(&[source_file("yarn.lock", src)]);

        assert!(findings
            .iter()
            .all(|f| f.rule != "high_entropy"));
        assert!(findings
            .iter()
            .any(|f| f.rule == "aws_access_key_id"));
    }

    #[test]
    fn content_digest_prefixes_never_flagged_as_high_entropy() {
        let src = "let expected = \"sha512-aB3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHiJkLmNoPqRsTuVwXyZ0aBcDeFgHiJkLmNoPq\";\n";
        let findings = high_entropy_findings(&[source_file("main.swift", src)]);

        assert!(findings.is_empty(), "findings: {:?}", findings);
    }

    #[test]
    fn findings_summary_groups_repeated_hits_into_one_entry() {
        let src = "let a = \"aB3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHi\";\n\
                   let b = \"aB3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHi\";\n\
                   let c = \"aB3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHi\";\n";
        let findings = scan_files(&[source_file("main.rs", src)]);
        let summary = super::findings_summary(&findings);

        assert_eq!(
            summary,
            "main.rs: high_entropy [WARN] ×3 (first at line 1) «REDACTED:aB3x...»"
        );
    }

    #[test]
    fn respects_slop_allow_secret_suppression() {
        let src = "let api = \"aB3xK9mN2pQr7sT4vUwXyZ0aBcDeFgHi\";  # slop:allow-secret\n";
        let findings = high_entropy_findings(&[source_file("main.rs", src)]);

        assert!(findings.is_empty(), "findings: {:?}", findings);
    }

    #[test]
    fn sensitive_filename_still_flagged_independently() {
        let findings = scan_files(&[source_file("secrets.rs", "// harmless\n")]);
        assert!(findings
            .iter()
            .any(|f| f.rule == "sensitive_filename" && f.severity == Severity::Block));
    }
}
