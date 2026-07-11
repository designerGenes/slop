use std::path::Path;

use super::ScoredPath;
use crate::error::SlopError;

pub const MAP_OVERHEAD: usize = 512;
pub const HEADER_RESERVE: usize = 1024;

pub fn estimate_map_reserve(config: &crate::config::Config) -> usize {
    config.graph_map_tokens * 4 + MAP_OVERHEAD
}

pub fn estimate_block_bytes(path: &Path) -> usize {
    let content = std::fs::read(path).unwrap_or_default();
    let content_len = content.len();
    let path_str = path.to_string_lossy();
    let header_len = format!(
        "#SLOP \"{}\" #SLOPED_LINES {} #SLOP_TRAILING_NEWLINE {}\n",
        path_str,
        content_len / 80,
        1
    )
    .len();
    content_len + header_len + 1
}

pub fn fill_budget(
    candidates: &[ScoredPath],
    max_slop_bytes: usize,
    map_reserve_bytes: usize,
    top_k: usize,
) -> Result<(Vec<ScoredPath>, Vec<ScoredPath>), SlopError> {
    let budget = max_slop_bytes
        .saturating_sub(map_reserve_bytes)
        .saturating_sub(HEADER_RESERVE);

    let mut selected: Vec<ScoredPath> = Vec::new();
    let mut dropped: Vec<ScoredPath> = Vec::new();
    let mut running_total = 0usize;

    for candidate in candidates {
        let blk = estimate_block_bytes(&candidate.path);

        if selected.is_empty() && running_total + blk > budget {
            return Err(SlopError::SoupBudgetExceeded {
                path: candidate.path.clone(),
                bytes: blk + map_reserve_bytes,
                cap: max_slop_bytes,
            });
        }

        if selected.len() < top_k && running_total + blk <= budget {
            running_total += blk;
            selected.push(candidate.clone());
        } else {
            dropped.push(candidate.clone());
        }
    }

    Ok((selected, dropped))
}

pub fn enforce_actual_budget(
    selected: &mut Vec<ScoredPath>,
    dropped: &mut Vec<ScoredPath>,
    actual_slop_bytes: usize,
    max_slop_bytes: usize,
) {
    while actual_slop_bytes > max_slop_bytes && selected.len() > 1 {
        let last = selected.pop().unwrap();
        dropped.push(last);
    }
}
