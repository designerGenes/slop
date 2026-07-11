use std::path::PathBuf;

use super::index;
use super::SelectionReason;
use crate::error::SlopError;

pub fn run_match_query(
    index: &tantivy::Index,
    reader: &tantivy::IndexReader,
    fields: &index::IndexFields,
    terms: &[String],
    top_k: usize,
) -> Result<Vec<super::ScoredPath>, SlopError> {
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let query_text = terms.join(" ");
    let results = index::query_index(index, reader, fields, &query_text, top_k)?;

    let mut scored: Vec<super::ScoredPath> = results
        .into_iter()
        .map(|(score, path, rel)| super::ScoredPath {
            path: PathBuf::from(path),
            rel_path: rel,
            score,
            reason: SelectionReason::Match,
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.rel_path.cmp(&b.rel_path))
    });
    Ok(scored)
}

pub fn run_task_query(
    index: &tantivy::Index,
    reader: &tantivy::IndexReader,
    fields: &index::IndexFields,
    task: &str,
    top_k: usize,
) -> Result<Vec<super::ScoredPath>, SlopError> {
    let results = index::query_index(index, reader, fields, task, top_k)?;

    let mut scored: Vec<super::ScoredPath> = results
        .into_iter()
        .map(|(score, path, rel)| super::ScoredPath {
            path: PathBuf::from(path),
            rel_path: rel,
            score,
            reason: SelectionReason::Task,
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.rel_path.cmp(&b.rel_path))
    });
    Ok(scored)
}
