//! Entry anchors: given a page's `--task` string, derive probe forms for the
//! entity being added and locate where a NEW entry of that kind would go, per
//! manifest file. Two mechanisms, one fallback ladder:
//!
//! 1. Insertion position — maximal sorted runs of tag names identify ordered
//!    registry blocks; when a probe form sorts inside one and matches both
//!    neighbours' tokens, the exact position is reported.
//! 2. Sibling exemplar — lines whose tags most resemble the probe by token
//!    overlap, chosen when a file is ordered by non-identifier keys (and
//!    sorted-block detection therefore has nothing to anchor on).
//!
//! Both operate only on the `StoredTag` name/line stream the project graph
//! already stores; nothing new is indexed. This is a port of the validated
//! Python prototypes in the FEATURES bundle, not a redesign.

use std::collections::BTreeSet;

/// One place a new entry either should be inserted (mechanism 1 annotation) or
/// simply looks like (mechanism 2 exemplar).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPoint {
    /// Anchor line: the predecessor line for insertion points, or the
    /// highest-scoring sibling line for exemplars.
    pub line: usize,
    /// Name of the entry on `line` (for insertion points).
    pub name: Option<String>,
    /// When present, the exact insertion position and its detected block.
    pub insertion: Option<Insertion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Insertion {
    pub before_name: String,
    pub before_line: usize,
    pub span_lo: usize,
    pub span_hi: usize,
}

/// Identifier-shaped tokens with their 1-based lines.
///
/// Deliberately NOT the tree-sitter tag stream. The upstream tags query covers
/// definitions and call/type references, so `use x::*;`, match arms keyed on
/// string literals, and attribute-macro arguments emit nothing -- and those are
/// the registry shapes entry anchors exist to serve. Extending the query is not
/// an option: tags feed symbol_edges, which feed the tower ranking every
/// benchmark number was tuned against.
pub fn scan_identifiers(text: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let mut current = String::new();
        for ch in line.chars() {
            if ch.is_alphanumeric() || ch == '_' {
                current.push(ch);
            } else if !current.is_empty() {
                push_identifier(&mut out, std::mem::take(&mut current), index + 1);
            }
        }
        if !current.is_empty() {
            push_identifier(&mut out, current, index + 1);
        }
    }
    out
}

fn push_identifier(out: &mut Vec<(String, usize)>, token: String, line: usize) {
    // An identifier cannot start with a digit; skip bare numbers so a match-arm
    // key like "051" does not enter the name stream.
    if token.starts_with(|c: char| c.is_ascii_digit()) {
        return;
    }
    out.push((token, line));
}

/// Split an identifier into comparable tokens at snake and camel boundaries.
///
/// A boundary is inserted where one genuinely exists: before an uppercase
/// character whose predecessor is lowercase or a digit, or whose predecessor
/// is uppercase and whose successor is lowercase. All-caps runs therefore
/// survive as one token: `PYI061` -> {pyi061}, `HTTPServer` -> {http, server}.
/// Tokens shorter than 3 chars are noise and discarded.
pub fn tokens_set(name: &str) -> BTreeSet<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut spaced = String::new();
    for (i, ch) in chars.iter().enumerate() {
        let boundary = ch.is_uppercase()
            && !spaced.is_empty()
            && match chars.get(i - 1) {
                Some(prev) => {
                    prev.is_lowercase()
                        || prev.is_ascii_digit()
                        || (prev.is_uppercase()
                            && chars.get(i + 1).is_some_and(|c| c.is_lowercase()))
                }
                None => false,
            };
        if boundary {
            spaced.push('_');
        }
        spaced.push(*ch);
    }
    spaced
        .split('_')
        .map(|part| part.to_lowercase())
        .filter(|part| part.len() >= 3)
        .collect()
}

fn shared_tokens(a: &str, b: &str) -> usize {
    let left = tokens_set(a);
    let right = tokens_set(b);
    left.intersection(&right).count()
}

/// Every casing the same entity wears across registries: snake_case for
/// modules, CamelCase for structs, and the bare token for codes.
///
/// Tokens inside backticks or quotes win over prose words when present — the
/// task string `[flake8-pyi] Implement `foo-bar` (`FB123`)` carries its entity
/// names inside the marks, and words like `Implement` only dilute the probe.
pub fn probe_forms(task: &str) -> Vec<String> {
    let mut phrases: Vec<String> = tokenize_grouped(task, |is| is == '`');
    if phrases.is_empty() {
        phrases = tokenize_grouped(task, |is| is == '"' || is == '\'');
    }
    if phrases.is_empty() {
        phrases.push(task.to_string());
    }
    let mut forms = Vec::new();
    for phrase in phrases {
        let words: Vec<&str> = phrase
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        if words.is_empty() {
            continue;
        }
        let snake = words.join("_").to_lowercase();
        let camel: String = words
            .iter()
            .map(|w| {
                let mut chs = w.chars();
                match chs.next() {
                    Some(first) => {
                        first.to_uppercase().collect::<String>() + &chs.as_str().to_lowercase()
                    }
                    None => String::new(),
                }
            })
            .collect();
        forms.push(snake);
        forms.push(camel);
        if words.len() == 1 {
            // A bare token form keeps the original casing: `PYI061` must stay
            // PYI061 to match registry keys and module map entries.
            forms.push(words[0].to_string());
        }
    }
    dedup(forms)
}

/// Split `task` into phrases between paired marks, keeping only marked groups.
/// A group that lives inside square brackets is a platform qualifier, not an
/// entity probe (`[`flake8-pyi`]`), and is deliberately skipped.
fn tokenize_grouped(task: &str, is_mark: impl Fn(char) -> bool) -> Vec<String> {
    let mut groups = Vec::new();
    let mut current = String::new();
    let mut inside = false;
    let mut brackets = 0usize;
    for ch in task.chars() {
        match ch {
            '[' | ']' if !inside => {
                if ch == '[' {
                    brackets += 1;
                } else {
                    brackets = brackets.saturating_sub(1);
                }
            }
            _ if is_mark(ch) => {
                if inside && !current.is_empty() {
                    if brackets == 0 {
                        groups.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                }
                inside = !inside;
            }
            _ if inside => current.push(ch),
            _ => {}
        }
    }
    groups
}

fn dedup(forms: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    forms
        .into_iter()
        .filter(|form| !form.is_empty())
        .filter(|form| seen.insert(form.clone()))
        .collect()
}

/// The plugin/platform qualifier named by the task — the group `probe_forms`
/// deliberately discards for anchoring.
///
/// Selection and anchoring want different probes. Filing a new rule is a
/// plugin-local act, so the plugin identity (`flake8-pyi` from
/// ``[`flake8-pyi`] Implement ...``) is what locates the registry files; the
/// entity name is near-useless for selection because a new name is lexically
/// unlike its siblings. Returns each bracketed mark group in order; if the
/// task has none, a leading mark group counts; otherwise empty.
pub fn qualifier_forms(task: &str) -> Vec<String> {
    let mut groups = Vec::new();
    let mut current = String::new();
    let mut inside = false;
    let mut brackets = 0usize;
    for ch in task.chars() {
        match ch {
            '[' | ']' if !inside => {
                if ch == '[' {
                    brackets += 1;
                } else {
                    brackets = brackets.saturating_sub(1);
                }
            }
            '`' => {
                if inside && !current.is_empty() {
                    if brackets > 0 {
                        groups.push(std::mem::take(&mut current));
                    } else {
                        current.clear();
                    }
                }
                inside = !inside;
            }
            _ if inside => current.push(ch),
            _ => {}
        }
    }
    if groups.is_empty() {
        let trimmed = task.trim_start();
        if let Some(rest) = trimmed.strip_prefix('`')
            && let Some(end) = rest.find('`')
            && !rest[..end].is_empty()
        {
            groups.push(rest[..end].to_string());
        }
    }
    dedup(groups)
}

/// Qualifier tokens, split with the same identifier rules and with `-` folded
/// to `_`: `flake8-pyi` -> {flake8, pyi}.
pub fn qualifier_tokens(task: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    for form in qualifier_forms(task) {
        tokens.extend(tokens_set(&form.replace('-', "_")));
    }
    tokens
}

/// Tokens over the path's `/`-separated segments, with `-` folded to `_` so a
/// directory named `flake8-pyi` still matches.
fn path_tokens(rel: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    for segment in rel.split('/') {
        tokens.extend(tokens_set(&segment.replace('-', "_")));
    }
    tokens
}

/// Whether the qualifier is carried by the file's path segments.
pub fn path_is_local(rel: &str, qualifier: &BTreeSet<String>) -> bool {
    !qualifier.is_empty() && path_tokens(rel).is_superset(qualifier)
}

/// Task-relevance score for one candidate file.
///
/// `content` counts identifiers in the file whose token set is a superset of
/// the qualifier's — registry entries that name the plugin (`Flake8Pyi`,
/// `flake8_pyi`). `registry_hint` covers the plugin's `rules/mod.rs`, which
/// lists rule modules but never names its own plugin: its path carries the
/// qualifier, and the name is exactly `mod.rs` — the module-registration
/// convention. Scoring every path-local file would flood the top with
/// individual rule files, so the bonus is restricted to `mod.rs`.
pub fn selection_score(text: &str, rel: &str, qualifier: &BTreeSet<String>) -> usize {
    if qualifier.is_empty() {
        return 0;
    }
    let content = scan_identifiers(text)
        .iter()
        .filter(|(name, _)| tokens_set(name).is_superset(qualifier))
        .count();
    let registry_hint = path_is_local(rel, qualifier) && rel.rsplit('/').next() == Some("mod.rs");
    content + if registry_hint { 10 } else { 0 }
}

/// Group tag names by source line, line numbers ascending.
fn by_line(tags: &[(String, usize)]) -> Vec<(usize, BTreeSet<String>)> {
    let mut lines: std::collections::BTreeMap<usize, BTreeSet<String>> = Default::default();
    for (name, line) in tags {
        lines.entry(*line).or_default().insert(name.clone());
    }
    lines.into_iter().collect()
}

/// Maximal line-contiguous runs along which a tag name can strictly increase.
///
/// Sortedness is an OBSERVED property, not a filename heuristic: a registry
/// keeps entries ordered, a logic file does not. Choosing the SMALLEST name
/// that continues the run makes the entity name win over incidental tokens
/// with no keyword list.
pub fn sorted_blocks(
    tags: &[(String, usize)],
    max_line_gap: usize,
    min_entries: usize,
) -> Vec<Vec<(String, usize)>> {
    let lines = by_line(tags);
    let mut blocks: Vec<Vec<(String, usize)>> = Vec::new();
    let mut run: Vec<(String, usize)> = Vec::new();

    fn close(blocks: &mut Vec<Vec<(String, usize)>>, run: &[(String, usize)], min_entries: usize) {
        if run.len() >= min_entries {
            blocks.push(run.to_vec());
        }
    }

    for (number, names) in lines {
        if let Some(prev) = run.last().cloned() {
            if number - prev.1 <= max_line_gap {
                let bigger = names.iter().filter(|name| *name > &prev.0).min().cloned();
                if let Some(bigger) = bigger {
                    run.push((bigger, number));
                    continue;
                }
            }
            close(&mut blocks, &run, min_entries);
            run.clear();
        }
        // Seed a new run with the line's smallest name: the entity name, when
        // any, rather than an incidental keyword.
        if let Some(least) = names.iter().min().cloned() {
            run.push((least, number));
        }
    }
    close(&mut blocks, &run, min_entries);
    blocks
}

/// Probe affinity with a neighbour. An insertion point between two unrelated
/// entries is worse than no anchor, so both neighbours must share tokens with
/// the probe; this is the condition that removed nearly all false positives
/// in validation.
fn affine(probe: &str, other: &str, min_shared: usize) -> bool {
    shared_tokens(probe, other) >= min_shared
}

/// Exact insertion positions for `probe` inside detected sorted blocks.
pub fn insertion_points(
    tags: &[(String, usize)],
    probe: &str,
    min_entries: usize,
    min_shared: usize,
    max_line_gap: usize,
) -> Vec<EntryPoint> {
    let blocks = sorted_blocks(tags, max_line_gap, min_entries);
    let mut points = Vec::new();
    for block in blocks {
        let names: Vec<&String> = block.iter().map(|(name, _)| name).collect();
        if names.iter().any(|name| *name == probe) {
            continue; // already registered
        }
        let lower: Vec<&(String, usize)> = block
            .iter()
            .filter(|(name, _)| name.as_str() < probe)
            .collect();
        let upper: Vec<&(String, usize)> = block
            .iter()
            .filter(|(name, _)| name.as_str() > probe)
            .collect();
        let (Some(after), Some(before)) = (lower.last(), upper.first()) else {
            continue; // probe sorts outside the run
        };
        if !(affine(probe, &after.0, min_shared) && affine(probe, &before.0, min_shared)) {
            continue; // neighbours are a different kind of thing
        }
        points.push(EntryPoint {
            line: after.1,
            name: Some(after.0.clone()),
            insertion: Some(Insertion {
                before_name: before.0.clone(),
                before_line: before.1,
                span_lo: block.first().expect("non-empty block").1,
                span_hi: block.last().expect("non-empty block").1,
            }),
        });
    }
    points
}

/// Sibling exemplars for the probe forms: the top-N lines whose tags most
/// resemble the entity being added. Ordering is irrelevant, so this finds
/// entries in files keyed by non-identifier strings — the case insertion
/// detection cannot serve and the case that matters most for big registries.
pub fn sibling_exemplars(
    tags: &[(String, usize)],
    forms: &[String],
    top: usize,
) -> Vec<EntryPoint> {
    let mut want: BTreeSet<String> = BTreeSet::new();
    for form in forms {
        want.extend(tokens_set(form));
    }
    if want.is_empty() || tags.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, usize)> = Vec::new(); // (score, line)
    let floor = std::cmp::min(2, want.len());
    for (number, names) in by_line(tags) {
        let best = names
            .iter()
            .filter_map(|name| {
                let own = tokens_set(name);
                let shared = own.intersection(&want).count();
                // A single shared token is not evidence of "the same kind of
                // entry": with the three-form probe, any line containing
                // `None` would otherwise anchor (`tokens_set("None") == {none}`).
                // One is accepted only when the probe itself has a single token
                // to share.
                (shared >= floor).then_some(shared)
            })
            .max()
            .unwrap_or(0);
        if best > 0 {
            scored.push((best, number));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored
        .into_iter()
        .take(top)
        .map(|(_, line)| EntryPoint {
            line,
            name: None,
            insertion: None,
        })
        .collect()
}

/// The public entry point: derive probes from the task and return ranked entry
/// points for one file's tag stream. Insertion positions win over sibling
/// exemplars when the file detectably orders its entries; a probe that is
/// already present is correctly reported as registered and contributes nothing.
pub fn entry_points(
    tags: &[(String, usize)],
    task: &str,
    max_points: usize,
    min_shared: usize,
    min_entries: usize,
    max_line_gap: usize,
) -> Vec<EntryPoint> {
    let forms = probe_forms(task);
    if forms.is_empty() || max_points == 0 {
        return Vec::new();
    }
    let mut combined: Vec<EntryPoint> = Vec::new();
    let mut seen = BTreeSet::new();
    for form in &forms {
        for point in insertion_points(tags, form, min_entries, min_shared, max_line_gap) {
            let key = (point.line, point.insertion.as_ref().map(|i| i.before_line));
            if seen.insert(key) {
                combined.push(point);
            }
        }
    }
    if combined.is_empty() {
        combined = sibling_exemplars(tags, &forms, max_points);
    }
    combined.truncate(max_points);
    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags(names: &[(&str, usize)]) -> Vec<(String, usize)> {
        names
            .iter()
            .map(|(name, line)| (name.to_string(), *line))
            .collect()
    }

    #[test]
    fn probe_forms_prefers_backtick_groups() {
        let forms = probe_forms("[flake8-pyi] Implement `redundant-none-literal` (`PYI061`)");
        assert!(
            forms.contains(&"redundant_none_literal".to_string()),
            "{forms:?}"
        );
        assert!(
            forms.contains(&"RedundantNoneLiteral".to_string()),
            "{forms:?}"
        );
        assert!(forms.contains(&"PYI061".to_string()), "{forms:?}");
        // Prose words like "Implement" must not leak into the joins.
        assert!(
            !forms.iter().any(|form| form.contains("implement")),
            "{forms:?}"
        );
    }

    #[test]
    fn probe_forms_falls_back_to_whole_task_without_marks() {
        let forms = probe_forms("Implement redundant-none-literal");
        assert!(
            forms.contains(&"implement_redundant_none_literal".to_string()),
            "{forms:?}"
        );
        assert!(
            forms.contains(&"ImplementRedundantNoneLiteral".to_string()),
            "{forms:?}"
        );
    }

    #[test]
    fn tokens_split_camel_and_snake() {
        let t = tokens_set("RedundantNoneLiteral");
        assert_eq!(
            t,
            ["redundant", "none", "literal"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        // All-caps runs stay whole; mixed runs split at true boundaries.
        assert_eq!(
            tokens_set("PYI061"),
            ["pyi061".to_string()].into_iter().collect()
        );
        assert_eq!(
            tokens_set("HTTPServer"),
            ["http", "server"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn qualifier_forms_takes_the_bracketed_group() {
        let forms = qualifier_forms("[`flake8-pyi`] Implement `redundant-none-literal` (`PYI061`)");
        assert_eq!(forms, vec!["flake8-pyi".to_string()]);
        assert!(qualifier_forms("implement redundant-none-literal").is_empty());
    }

    #[test]
    fn qualifier_forms_falls_back_to_a_leading_group() {
        let forms = qualifier_forms("`airflow` implement task-thing");
        assert_eq!(forms, vec!["airflow".to_string()]);
    }

    #[test]
    fn qualifier_tokens_fold_dashes_and_split_camel() {
        let tokens = qualifier_tokens("[`flake8-pyi`] Implement `x`");
        assert_eq!(
            tokens,
            ["flake8", "pyi"]
                .iter()
                .map(|token| token.to_string())
                .collect()
        );
    }

    #[test]
    fn selection_score_counts_identifiers_that_carry_the_qualifier() {
        let qualifier = qualifier_tokens("[`flake8-pyi`] Implement `x`");
        let text = "let a = Flake8Pyi;\nfn f() { flake8_pyi::rules(); }\n";
        assert_eq!(
            selection_score(text, "crates/ruff_linter/src/codes.rs", &qualifier),
            2
        );
    }

    #[test]
    fn selection_score_covers_the_plugin_mod_rs_via_its_path() {
        let qualifier = qualifier_tokens("[`flake8-pyi`] Implement `x`");
        let text = "mod alpha;\nmod beta;\n";
        assert_eq!(
            selection_score(
                text,
                "crates/ruff_linter/src/rules/flake8_pyi/rules/mod.rs",
                &qualifier
            ),
            10
        );
        // Content and the registry hint add up.
        assert_eq!(
            selection_score(
                "use flake8_pyi;\n",
                "crates/ruff_linter/src/rules/flake8_pyi/rules/mod.rs",
                &qualifier
            ),
            11
        );
        // A path-local rule file gets no hint; an unrelated mod.rs neither.
        assert_eq!(
            selection_score(
                text,
                "crates/ruff_linter/src/rules/flake8_pyi/rules/alpha.rs",
                &qualifier
            ),
            0
        );
        assert_eq!(
            selection_score(
                text,
                "crates/ruff_linter/src/rules/other/mod.rs",
                &qualifier
            ),
            0
        );
    }

    #[test]
    fn no_qualifier_scores_zero_everywhere() {
        let qualifier = qualifier_tokens("implement redundant-none-literal");
        assert!(qualifier.is_empty());
        assert_eq!(
            selection_score("Flake8Pyi flake8_pyi", "flake8_pyi/mod.rs", &qualifier),
            0
        );
    }

    #[test]
    fn scan_identifiers_reports_tokens_with_lines() {
        let text = "pub(crate) use redundant_literal_union::*;\n(Flake8Pyi, \"051\") => RuleGroup::Stable,\n";
        let scanned = scan_identifiers(text);
        assert!(
            scanned.contains(&("redundant_literal_union".to_string(), 1)),
            "{scanned:?}"
        );
        assert!(
            scanned.contains(&("Flake8Pyi".to_string(), 2)),
            "{scanned:?}"
        );
        assert!(
            scanned.contains(&("RuleGroup".to_string(), 2)),
            "{scanned:?}"
        );
        // Bare numeric keys must not enter the name stream.
        assert!(
            !scanned.iter().any(|(name, _)| name == "051"),
            "{scanned:?}"
        );
    }

    #[test]
    fn two_independent_sorted_blocks_yield_two_insertion_points() {
        // The rules/mod.rs shape: a `use` block and a `mod` block, both sorted.
        let tags = tags(&[
            ("quoted_annotation_in_stub", 27),
            ("redundant_final_literal", 28),
            ("redundant_literal_union", 29),
            ("redundant_numeric_union", 30),
            ("simple_defaults", 31),
            ("str_or_repr_defined_in_stub", 32),
            ("quoted_annotation_in_stub", 69),
            ("redundant_final_literal", 70),
            ("redundant_literal_union", 71),
            ("redundant_numeric_union", 72),
            ("simple_defaults", 73),
            ("str_or_repr_defined_in_stub", 74),
        ]);
        let points = insertion_points(&tags, "redundant_none_literal", 4, 1, 2);
        assert_eq!(points.len(), 2, "points {points:?}");
        assert_eq!(points[0].line, 29);
        assert_eq!(points[0].insertion.as_ref().unwrap().before_line, 30);
        assert_eq!(points[1].line, 71);
        assert_eq!(points[1].insertion.as_ref().unwrap().before_line, 72);
    }

    #[test]
    fn probe_neighbours_without_shared_tokens_yield_no_point() {
        let tags = tags(&[
            ("alpha_settings", 10),
            ("beta_common", 11),
            ("delta_window", 12),
            ("epsilon_config", 13),
        ]);
        let points = insertion_points(&tags, "GammaBreach", 4, 1, 2);
        assert!(points.is_empty(), "points {points:?}");
    }

    #[test]
    fn probe_already_present_is_skipped_as_registered() {
        let tags = tags(&[
            ("quoted_annotation_in_stub", 27),
            ("redundant_final_literal", 28),
            ("redundant_literal_union", 29),
            ("redundant_numeric_union", 30),
        ]);
        let points = insertion_points(&tags, "redundant_literal_union", 4, 1, 2);
        assert!(points.is_empty(), "points {points:?}");
    }

    #[test]
    fn multi_line_gap_run_is_not_one_block() {
        let tags = tags(&[
            ("alpha", 10),
            ("beta", 11),
            ("gamma", 12),
            ("delta", 13),
            ("epsilon", 25),
            ("zeta", 26),
            ("eta", 27),
            ("theta", 28),
        ]);
        // With a gap of 12 lines, neither run reaches min_entries=4 alone under
        // max_line_gap=2... actually each run here has 4 entries, so both are
        // blocks; the point is that the gap closes the first block before the
        // second starts. A probe between the two halves must anchor in ONE of
        // them, not straddle the gap.
        let points = insertion_points(&tags, "zeta", 4, 1, 2);
        assert!(
            !points
                .iter()
                .any(|p| p.insertion.as_ref().is_some_and(|i| i.span_lo < 25)),
            "point crossed the gap: {points:?}"
        );
    }

    #[test]
    fn unsorted_symbols_yield_no_insertion_point() {
        let tags = tags(&[
            ("parse_config", 10),
            ("render_page", 11),
            ("compute_hash", 12),
            ("validate_input", 13),
            ("emit_report", 14),
        ]);
        let points = insertion_points(&tags, "validate_output", 4, 1, 2);
        assert!(points.is_empty(), "{points:?}");
    }

    #[test]
    fn exemplar_matches_sibling_entries_by_token_overlap() {
        let tags = tags(&[
            ("RedundantLiteralUnion", 780),
            ("RedundantFinalLiteral", 781),
            ("unrelated_helper", 782),
            ("NoReturnArgumentAnnotationInStub", 783),
            ("RedundantNumericUnion", 784),
        ]);
        let points = sibling_exemplars(&tags, &["RedundantNoneLiteral".to_string()], 2);
        let lines: Vec<usize> = points.iter().map(|p| p.line).collect();
        assert_eq!(lines, vec![780, 781], "{points:?}");
    }

    #[test]
    fn exemplar_rejects_a_line_whose_only_overlap_is_none() {
        let tags = tags(&[
            ("None", 5),
            ("RedundantLiteralUnion", 780),
            ("RedundantFinalLiteral", 781),
        ]);
        let points = sibling_exemplars(&tags, &["RedundantNoneLiteral".to_string()], 2);
        assert!(
            !points.iter().any(|point| point.line == 5),
            "a line whose only shared token is `None` must not anchor: {points:?}"
        );
        assert_eq!(
            points.iter().map(|point| point.line).collect::<Vec<_>>(),
            vec![780, 781]
        );
    }

    #[test]
    fn entry_points_ranks_insertion_above_exemplar() {
        let tags = tags(&[
            ("quoted_annotation_in_stub", 27),
            ("redundant_final_literal", 28),
            ("redundant_literal_union", 29),
            ("redundant_numeric_union", 30),
        ]);
        let points = entry_points(&tags, "redundant-none-literal", 1, 1, 4, 2);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].line, 29, "{points:?}");
        assert_eq!(
            points[0].insertion.as_ref().map(|i| i.before_line),
            Some(30),
            "{points:?}"
        );
    }

    #[test]
    fn quick_smoke_for_max_points_cap() {
        let tags = tags(&[
            ("alpha_delta_redundant", 1),
            ("beta_delta_redundant", 2),
            ("gamma_delta_redundant", 3),
        ]);
        let points = sibling_exemplars(&tags, &["delta_redundant".to_string()], 2);
        assert_eq!(points.len(), 2);
    }

    /// Step 1 of the entry-anchor validation, now against the raw identifier
    /// scan that brief-2 settled on. Asserts the published expected values for
    /// the three registry files at ruff `4a2310b595` when a checkout is named
    /// by env, and otherwise skips silently.
    #[test]
    fn real_ruff_scan_matches_validated_results() {
        let Ok(root) = std::env::var("ANCHOR_VALIDATE_RUFF_ROOT") else {
            return;
        };
        let task = "[`flake8-pyi`] Implement `redundant-none-literal` (`PYI061`)";
        let scan = |rel: &str| {
            let abs = std::path::Path::new(&root).join(rel);
            let text = std::fs::read_to_string(&abs).expect("read fixture");
            scan_identifiers(&text)
        };

        let rules_mod = "crates/ruff_linter/src/rules/flake8_pyi/rules/mod.rs";
        let points = entry_points(&scan(rules_mod), task, 2, 1, 4, 2);
        let positions: Vec<(usize, usize)> = points
            .iter()
            .map(|p| (p.line, p.insertion.as_ref().expect("insertion").before_line))
            .collect();
        assert_eq!(positions, [(29, 30), (71, 72)], "rules/mod.rs: {points:?}");

        let pyi_mod = "crates/ruff_linter/src/rules/flake8_pyi/mod.rs";
        let points = entry_points(&scan(pyi_mod), task, 2, 1, 4, 2);
        let before_line = points[0]
            .insertion
            .as_ref()
            .expect("pytest_case insertion")
            .before_line;
        assert_eq!(before_line, 75, "flake8_pyi/mod.rs: {points:?}");

        let codes = "crates/ruff_linter/src/codes.rs";
        let points = entry_points(&scan(codes), task, 2, 1, 4, 2);
        assert!(
            points.iter().all(|p| p.insertion.is_none()),
            "codes.rs must not anchor inside a sorted block: {points:?}"
        );
        assert_eq!(points[0].line, 780, "codes.rs exemplar line: {points:?}");
    }
}
