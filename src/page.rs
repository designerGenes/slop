use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::SlopError;
use crate::graph::find_git_root;
use crate::graphstore::{
    self, Tier,
    page::{self, PAGE_SCHEMA, PageAddReason, PageFileState, PageManifest, PageStatus},
};
use crate::models::{CliArgs, SoupMetaBlock};
use crate::pathing::{normalize_path, resolve_absolute};
use crate::slop::build_source_file;
use crate::slop_format::{parse_document, serialize_document};
use crate::tower_graph::resolve_tower_seeds;

pub fn run(args: &CliArgs, config: &Config) -> Result<(), SlopError> {
    if args.page_open {
        open(args, config)
    } else if args.page_add {
        add(args, config)
    } else if args.page_close {
        close(args, config)
    } else if args.page_list {
        list(config)
    } else {
        prune(args, config)
    }
}

fn open(args: &CliArgs, config: &Config) -> Result<(), SlopError> {
    let (root, seeds) = resolve_tower_seeds(args, config)?;
    let tower = graphstore::refresh_tower_graph(&root, &seeds, config, args.reindex)?;
    let now = now_unix();
    let page_id = format!(
        "{:013x}-{}",
        now * 1_000,
        &blake3::hash(format!("{}:{:?}", root.display(), seeds).as_bytes()).to_hex()[..8]
    );
    let mut files = Vec::new();
    let mut states = Vec::new();
    for member in &tower.members {
        if member.tier == Tier::Zero || (member.tier == Tier::One && config.page_tier1_include) {
            let path = root.join(&member.rel);
            let source = build_source_file(&path)?;
            states.push(PageFileState {
                rel: member.rel.clone(),
                tier: member.tier,
                base_sha: source.base_sha.clone().expect("page source has SHA"),
                added_via: PageAddReason::Opened,
            });
            files.push(source);
        }
    }
    files = crate::secrets::enforce(&files, config, false, false)?;
    let manifest = PageManifest {
        schema: PAGE_SCHEMA,
        page_id: page_id.clone(),
        repo_id: tower.repo_id.clone(),
        repo_root: root.to_string_lossy().to_string(),
        task: args.task.clone(),
        status: PageStatus::Open,
        seed_digest: tower.seed_digest.clone(),
        opened_at_unix: now,
        closed_at_unix: None,
        files: states,
    };
    let meta = context_meta(
        &manifest,
        &tower,
        &graphstore::refresh_project_graph(&root, config, false)?.0,
    );
    let context = page::context_path(config, &manifest.repo_id, &page_id);
    fs::create_dir_all(context.parent().expect("context parent")).map_err(|source| {
        SlopError::DirectoryCreationFailure {
            path: context.parent().unwrap().to_path_buf(),
            source,
        }
    })?;
    fs::write(&context, serialize_document(&[meta], &files)?).map_err(|source| {
        SlopError::FileWriteFailure {
            path: context.clone(),
            source,
        }
    })?;
    page::save_page(config, &manifest)?;
    if !args.silent {
        eprintln!("page {}: {}", page_id, context.display());
    }
    Ok(())
}

fn add(args: &CliArgs, config: &Config) -> Result<(), SlopError> {
    let (root, repo_id) = current_repo(config)?;
    let mut manifest = select_page(config, &repo_id, args.page_id.as_deref())?;
    if manifest.status == PageStatus::Closed {
        return Err(SlopError::PageAlreadyClosed(manifest.page_id));
    }
    let tower = graphstore::store::load_tower_graph(&graphstore::store::tower_graph_path(
        config,
        &root,
        &manifest.seed_digest,
    ))
    .ok_or_else(|| {
        SlopError::GraphStoreFailure("the page's tower graph is unavailable".to_string())
    })?;
    let cwd = std::env::current_dir().map_err(|source| SlopError::FileReadFailure {
        path: PathBuf::from("."),
        source,
    })?;
    let mut document = parse_document(
        &fs::read_to_string(page::context_path(config, &repo_id, &manifest.page_id)).map_err(
            |source| SlopError::FileReadFailure {
                path: page::context_path(config, &repo_id, &manifest.page_id),
                source,
            },
        )?,
    )?;
    let mut sources = document
        .blocks
        .into_iter()
        .map(|block| crate::models::SourceFile {
            original_absolute_path: block.original_absolute_path,
            file_name: String::new(),
            name_token: String::new(),
            contents: reconstruct(&block.content_lines, block.trailing_newline),
            logical_line_count: block.logical_line_count,
            trailing_newline: block.trailing_newline,
            base_sha: block.base_sha,
            read_only: block.read_only,
        })
        .collect::<Vec<_>>();
    for input in &args.inputs {
        let path = resolve_absolute(input, &cwd)?;
        let rel = path
            .strip_prefix(&root)
            .map_err(|_| SlopError::TowerSeedOutsideRepo(path.clone()))?
            .to_string_lossy()
            .replace('\\', "/");
        if manifest.files.iter().any(|file| file.rel == rel) {
            eprintln!("warning: {rel} is already in page {}", manifest.page_id);
            continue;
        }
        let source = build_source_file(&path)?;
        let mut checked =
            crate::secrets::enforce(std::slice::from_ref(&source), config, false, false)?;
        let source = checked
            .pop()
            .expect("one source remains after secret enforcement");
        let tier = tower
            .members
            .iter()
            .find(|member| member.rel == rel)
            .map(|member| member.tier)
            .unwrap_or(Tier::Three);
        let reason = if tower.members.iter().any(|member| member.rel == rel) {
            PageAddReason::Promoted
        } else {
            PageAddReason::Requested
        };
        manifest.files.push(PageFileState {
            rel,
            tier,
            base_sha: source.base_sha.clone().expect("page source has SHA"),
            added_via: reason,
        });
        sources.push(source);
    }
    let project = graphstore::refresh_project_graph(&root, config, false)?.0;
    document
        .meta_blocks
        .retain(|meta| meta.kind != "context-page");
    document
        .meta_blocks
        .insert(0, context_meta(&manifest, &tower, &project));
    fs::write(
        page::context_path(config, &repo_id, &manifest.page_id),
        serialize_document(&document.meta_blocks, &sources)?,
    )
    .map_err(|source| SlopError::FileWriteFailure {
        path: page::context_path(config, &repo_id, &manifest.page_id),
        source,
    })?;
    page::save_page(config, &manifest)
}

fn close(args: &CliArgs, config: &Config) -> Result<(), SlopError> {
    let (root, repo_id) = current_repo(config)?;
    let mut manifest = select_page(config, &repo_id, args.page_id.as_deref())?;
    if manifest.status == PageStatus::Closed {
        return Err(SlopError::PageAlreadyClosed(manifest.page_id));
    }
    let allowed: BTreeSet<PathBuf> = manifest
        .files
        .iter()
        .map(|file| normalize_path(&root.join(&file.rel)))
        .collect();
    let returned = page::page_dir(config, &repo_id, &manifest.page_id).join("returned");
    if let Ok(entries) = fs::read_dir(returned) {
        for entry in entries.filter_map(Result::ok) {
            let content =
                fs::read_to_string(entry.path()).map_err(|source| SlopError::FileReadFailure {
                    path: entry.path(),
                    source,
                })?;
            let document = parse_document(&content)?;
            for block in &document.blocks {
                let path = normalize_path(&block.original_absolute_path);
                if !allowed.contains(&path) {
                    return Err(SlopError::PageWriteOutsideScope {
                        path,
                        page: manifest.page_id.clone(),
                    });
                }
            }
            crate::deslop::apply_document(document, args, config, Some(&allowed))?;
        }
    }
    let (_, report) = graphstore::refresh_project_graph(&root, config, false)?;
    manifest.status = PageStatus::Closed;
    manifest.closed_at_unix = Some(now_unix());
    page::save_page(config, &manifest)?;
    if !args.silent {
        eprintln!("page {} closed; {}", manifest.page_id, report.summary());
    }
    Ok(())
}

fn list(config: &Config) -> Result<(), SlopError> {
    let root = page::pages_dir(config);
    let Ok(repos) = fs::read_dir(&root) else {
        return Ok(());
    };
    for repo in repos.filter_map(Result::ok) {
        if let Ok(pages) = fs::read_dir(repo.path()) {
            for entry in pages.filter_map(Result::ok) {
                if let Some(manifest) = page::load_page(&entry.path().join("page.json")) {
                    println!(
                        "{}\t{}\t{:?}",
                        manifest.repo_id, manifest.page_id, manifest.status
                    );
                }
            }
        }
    }
    Ok(())
}

fn prune(args: &CliArgs, config: &Config) -> Result<(), SlopError> {
    let age = parse_duration(
        args.older_than
            .as_deref()
            .unwrap_or(&config.page_prune_after),
    )?;
    let now = now_unix();
    let root = page::pages_dir(config);
    if let Ok(repos) = fs::read_dir(&root) {
        for repo in repos.filter_map(Result::ok) {
            if let Ok(entries) = fs::read_dir(repo.path()) {
                for entry in entries.filter_map(Result::ok) {
                    if let Some(manifest) = page::load_page(&entry.path().join("page.json"))
                        && manifest.status == PageStatus::Closed
                        && manifest
                            .closed_at_unix
                            .is_some_and(|closed| now.saturating_sub(closed) > age)
                    {
                        fs::remove_dir_all(entry.path()).map_err(|source| {
                            SlopError::FileWriteFailure {
                                path: entry.path(),
                                source,
                            }
                        })?;
                    }
                }
            }
        }
    }
    Ok(())
}

fn current_repo(_config: &Config) -> Result<(PathBuf, String), SlopError> {
    let cwd = std::env::current_dir().map_err(|source| SlopError::FileReadFailure {
        path: PathBuf::from("."),
        source,
    })?;
    let root = find_git_root(&cwd).ok_or(SlopError::GraphRepoRootUnresolved(cwd))?;
    Ok((root.clone(), graphstore::store::repo_id(&root)))
}
fn select_page(
    config: &Config,
    repo_id: &str,
    requested: Option<&str>,
) -> Result<PageManifest, SlopError> {
    if let Some(id) = requested {
        return page::load_page(&page::manifest_path(config, repo_id, id))
            .ok_or_else(|| SlopError::PageNotFound(id.to_string()));
    }
    let pages = page::open_pages(config, repo_id);
    match pages.len() {
        0 => Err(SlopError::NoOpenPage),
        1 => Ok(pages.into_iter().next().unwrap()),
        _ => Err(SlopError::AmbiguousOpenPage(
            pages.into_iter().map(|page| page.page_id).collect(),
        )),
    }
}
fn context_meta(
    manifest: &PageManifest,
    tower: &graphstore::TowerGraph,
    project: &graphstore::ProjectGraph,
) -> SoupMetaBlock {
    let bundled: BTreeSet<&str> = manifest
        .files
        .iter()
        .map(|file| file.rel.as_str())
        .collect();
    let mut lines = vec![
        "# CONTEXT PAGE".to_string(),
        format!(
            "# page: {}   task: {}",
            manifest.page_id,
            manifest.task.as_deref().unwrap_or("(none)")
        ),
        format!("# repo: {}", manifest.repo_root),
        "#".to_string(),
        format!(
            "# TIER 0/1 (bundled below, full text): {}",
            manifest
                .files
                .iter()
                .map(|file| file.rel.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "#".to_string(),
        "# TIER 2 (outline only - request the file to see full text):".to_string(),
    ];
    for member in tower
        .members
        .iter()
        .filter(|member| member.tier == Tier::Two && !bundled.contains(member.rel.as_str()))
    {
        let defs = project
            .file(&member.rel)
            .map(|file| {
                file.def_tags()
                    .take(8)
                    .map(|tag| tag.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        lines.push(format!("#   {}   defines: {}", member.rel, defs));
    }
    lines.push("#".to_string());
    lines.push("# TIER 3 (name only):".to_string());
    for member in tower
        .members
        .iter()
        .filter(|member| member.tier == Tier::Three && !bundled.contains(member.rel.as_str()))
    {
        lines.push(format!("#   {}", member.rel));
    }
    lines.extend(["#".to_string(), "# To pull a tier-2/3 file into full text, emit: #SLOP_REQUEST \"<absolute path>\" <reason>".to_string()]);
    SoupMetaBlock {
        label: "context-page".to_string(),
        kind: "context-page".to_string(),
        format: "text".to_string(),
        line_count: lines.len(),
        readonly: true,
        content_lines: lines,
    }
}
fn reconstruct(lines: &[String], trailing: bool) -> String {
    let mut result = lines.join("\n");
    if trailing {
        result.push('\n');
    }
    result
}
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn parse_duration(raw: &str) -> Result<u64, SlopError> {
    let (number, unit) = raw.split_at(raw.len().saturating_sub(1));
    let number = number
        .parse::<u64>()
        .map_err(|_| SlopError::InvalidCliUsage(format!("invalid duration: {raw}")))?;
    match unit {
        "m" => Ok(number * 60),
        "h" => Ok(number * 3600),
        "d" => Ok(number * 86400),
        _ => Err(SlopError::InvalidCliUsage(format!(
            "invalid duration: {raw}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphstore::{TowerGraph, TowerMember};

    fn manifest() -> PageManifest {
        PageManifest {
            schema: PAGE_SCHEMA,
            page_id: "001-test".to_string(),
            repo_id: "repo".to_string(),
            repo_root: "/repo".to_string(),
            task: None,
            status: PageStatus::Open,
            seed_digest: "digest".to_string(),
            opened_at_unix: 0,
            closed_at_unix: None,
            files: vec![PageFileState {
                rel: "a.rs".to_string(),
                tier: Tier::Zero,
                base_sha: "0".repeat(64),
                added_via: PageAddReason::Opened,
            }],
        }
    }

    #[test]
    fn context_metadata_outlines_unbundled_lower_tiers() {
        let tower = TowerGraph {
            schema: 1,
            repo_id: "repo".to_string(),
            seed_digest: "digest".to_string(),
            seeds: vec!["a.rs".to_string()],
            project_graph_fingerprint: "fingerprint".to_string(),
            generated_at_unix: 0,
            members: vec![
                TowerMember {
                    rel: "a.rs".to_string(),
                    tier: Tier::Zero,
                    score: 1.0,
                    via: Vec::new(),
                },
                TowerMember {
                    rel: "b.rs".to_string(),
                    tier: Tier::Two,
                    score: 0.2,
                    via: vec!["a.rs".to_string()],
                },
                TowerMember {
                    rel: "c.rs".to_string(),
                    tier: Tier::Three,
                    score: 0.1,
                    via: Vec::new(),
                },
            ],
            cut_scores: [0.0; 3],
        };
        let project = crate::graphstore::ProjectGraph {
            schema: 1,
            repo_root: "/repo".to_string(),
            repo_id: "repo".to_string(),
            generated_at_unix: 0,
            generator_version: "test".to_string(),
            files: Vec::new(),
            symbol_edges: Vec::new(),
            cochange_edges: Vec::new(),
            communities: Vec::new(),
            structure: Default::default(),
            stats: Default::default(),
        };
        let meta = context_meta(&manifest(), &tower, &project);
        assert!(meta.readonly);
        assert!(meta.content_lines.iter().any(|line| line.contains("b.rs")));
        assert!(meta.content_lines.iter().any(|line| line.contains("c.rs")));
    }

    #[test]
    fn accepts_only_documented_prune_durations() {
        assert_eq!(parse_duration("7d").expect("days"), 604_800);
        assert_eq!(parse_duration("30m").expect("minutes"), 1_800);
        assert!(parse_duration("forever").is_err());
    }
}
