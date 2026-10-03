use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::Config;
use crate::error::SlopError;
use crate::graph::find_git_root;
use crate::graphstore::{
    self, Tier,
    page::{
        self, PAGE_SCHEMA, PageAddReason, PageCloseChange, PageCloseSource, PageDelivery,
        PageFileState, PageManifest, PageStatus,
    },
};
use crate::models::{CliArgs, SoupMetaBlock};
use crate::pathing::{canonicalize_path, resolve_absolute};
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

/// One manifest-page selection decision: indices into `tower.members`, in page
/// order, plus which of them were admitted through the reserved
/// task-relevance slots.
#[derive(Debug)]
pub(crate) struct PageSelection {
    pub(crate) indices: Vec<usize>,
    pub(crate) promoted: BTreeSet<usize>,
    /// Which signal actually admitted the promoted files: `"history"`,
    /// `"qualifier"`, or `"legacy"`.
    pub(crate) signal: &'static str,
    /// Prior sibling-addition commits mined, when the history signal was used.
    pub(crate) prior_commits: Option<usize>,
}

/// Choose the manifest page's files.
///
/// Without task-relevance promotion this is exactly the legacy behaviour: the
/// first `page_manifest_max_files` tower members. With it, the seed stays
/// first, up to `page_task_relevance_reserved_slots` files selected by the
/// history signal (or the qualifier scorer when history is unusable) follow,
/// and the remainder is filled from the existing tower order. Promotion
/// reserves slots rather than altering scores, so it cannot reshape the
/// ranking — only displace a bounded number of files.
pub(crate) fn select_manifest_page(
    tower: &graphstore::TowerGraph,
    config: &Config,
    task: Option<&str>,
    repo_root: &Path,
    verbose: bool,
) -> PageSelection {
    let cap = config.page_manifest_max_files.max(1);
    let legacy = || PageSelection {
        indices: (0..tower.members.len().min(cap)).collect(),
        promoted: BTreeSet::new(),
        signal: "legacy",
        prior_commits: None,
    };
    if !config.page_task_relevance_promotion {
        return legacy();
    }
    let Some(task) = task.filter(|task| !task.trim().is_empty()) else {
        return legacy();
    };

    // Primary signal: what did prior sibling additions modify? The repository
    // has already answered "which files does adding a new X touch" every time
    // someone added the previous X. No naming assumptions, no fitted weights.
    if let Some(seed) = tower.seeds.first()
        && let Some(history) = crate::history_select::sibling_additions(
            repo_root,
            seed,
            config.page_history_commit_window,
            None,
        )
    {
        let mut ranked: Vec<(usize, usize)> = Vec::new(); // (tally, index)
        for (index, member) in tower.members.iter().enumerate() {
            if index < cap {
                continue; // already inside the page window
            }
            if let Some(&count) = history.tally.get(&member.rel)
                && history.meets_minimum(&member.rel)
            {
                ranked.push((count, index));
            }
        }
        ranked.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
        let promoted: Vec<usize> = ranked
            .iter()
            .take(config.page_task_relevance_reserved_slots)
            .map(|(_, index)| *index)
            .collect();
        if verbose {
            eprintln!(
                "page task relevance: signal history ({} prior sibling-additions), {} candidates, {} promoted",
                history.prior_commits,
                ranked.len(),
                promoted.len()
            );
            for &(count, index) in ranked
                .iter()
                .take(config.page_task_relevance_reserved_slots)
            {
                eprintln!(
                    "  promoted: {count}/{}  {}",
                    history.prior_commits, tower.members[index].rel
                );
            }
        }
        if !promoted.is_empty() {
            return page_from_promoted(
                tower,
                cap,
                &promoted,
                "history",
                Some(history.prior_commits),
            );
        }
    }

    // Fallback for repositories with no usable history: the qualifier probe.
    let qualifier = crate::anchor::qualifier_tokens(task);
    if qualifier.is_empty() {
        return legacy();
    }

    let mut candidates: Vec<(usize, usize)> = Vec::new(); // (score, index)
    let mut examined = 0usize;
    let mut rejected = 0usize;
    let mut scanned = 0usize;
    for (index, member) in tower.members.iter().enumerate() {
        if index < cap {
            continue; // already inside the page window
        }
        // The bound caps the expensive work — files that survive the
        // pre-filter and get scored — not the cheap reads, so a registry at
        // rank 988 is still reachable on a large repo.
        if scanned >= config.page_task_relevance_max_candidates {
            break;
        }
        examined += 1;
        let Ok(text) = fs::read_to_string(repo_root.join(&member.rel)) else {
            continue;
        };
        // Plain substring pre-filter for any qualifier token, plus the cheap
        // path check: a file with neither cannot score.
        let lowered = text.to_ascii_lowercase();
        let text_hit = qualifier
            .iter()
            .any(|token| lowered.contains(token.as_str()));
        if !text_hit && !crate::anchor::path_is_local(&member.rel, &qualifier) {
            rejected += 1;
            continue;
        }
        scanned += 1;
        let score = crate::anchor::selection_score(&text, &member.rel, &qualifier);
        if score > 0 {
            candidates.push((score, index));
        }
    }
    // Rank by evidence strength, then tower order as the tie-break. The old
    // first-come walk let whichever low-score file matched first take a slot.
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    let promoted: Vec<usize> = candidates
        .iter()
        .take(config.page_task_relevance_reserved_slots)
        .map(|(_, index)| *index)
        .collect();
    if verbose {
        eprintln!(
            "page task relevance: signal qualifier fallback; examined {examined}, pre-filter rejected {rejected}, scanned {scanned}, promoted {}",
            promoted.len()
        );
        for &(score, index) in candidates
            .iter()
            .take(config.page_task_relevance_reserved_slots)
        {
            eprintln!("  promoted: score {score}  {}", tower.members[index].rel);
        }
    }
    if promoted.is_empty() {
        return legacy();
    }
    page_from_promoted(tower, cap, &promoted, "qualifier", None)
}

/// The seed, then the reserved-slot files, then the existing tower order to
/// the cap, skipping duplicates.
fn page_from_promoted(
    tower: &graphstore::TowerGraph,
    cap: usize,
    promoted: &[usize],
    signal: &'static str,
    prior_commits: Option<usize>,
) -> PageSelection {
    let promoted_set: BTreeSet<usize> = promoted.iter().copied().collect();
    let mut indices: Vec<usize> = Vec::with_capacity(cap);
    for (index, member) in tower.members.iter().enumerate() {
        if member.tier == Tier::Zero && indices.len() < cap {
            indices.push(index);
        }
    }
    for &index in promoted {
        if indices.len() < cap {
            indices.push(index);
        }
    }
    for index in 0..tower.members.len() {
        if indices.len() >= cap {
            break;
        }
        if promoted_set.contains(&index) || tower.members[index].tier == Tier::Zero {
            continue;
        }
        indices.push(index);
    }
    PageSelection {
        indices,
        promoted: promoted_set,
        signal,
        prior_commits,
    }
}

fn open(args: &CliArgs, config: &Config) -> Result<(), SlopError> {
    let (root, seeds) = resolve_tower_seeds(args, config)?;
    // `resolve_tower_seeds` has just refreshed and persisted the project graph.
    // Build and save the cheap tower directly so page-open pays for one slop
    // invocation and page-add can load the exact same ranking later.
    let project = graphstore::load_project_graph(&root, config).ok_or_else(|| {
        SlopError::GraphStoreFailure(format!("project graph disappeared for {}", root.display()))
    })?;
    let tower = graphstore::build_tower_graph(&project, &seeds, config);
    graphstore::store::save_tower_graph(
        &graphstore::store::tower_graph_path(config, &root, &tower.seed_digest),
        &tower,
    )?;
    let now = now_unix();
    let page_id = allocate_page_id(config, &tower.repo_id, &root, &seeds)?;
    let mut manifest = PageManifest {
        schema: PAGE_SCHEMA,
        page_id: page_id.clone(),
        repo_id: tower.repo_id.clone(),
        repo_root: root.to_string_lossy().to_string(),
        task: args.task.clone(),
        base_git_head: git_head(&root),
        status: PageStatus::Open,
        seed_digest: tower.seed_digest.clone(),
        opened_at_unix: now,
        closed_at_unix: None,
        delivery: if args.page_manifest {
            PageDelivery::Manifest
        } else {
            PageDelivery::Bundle
        },
        files: Vec::new(),
        closed_changes: Vec::new(),
    };
    let context = page::context_path(config, &manifest.repo_id, &page_id);
    let entry_env = EntryEnv {
        config,
        task: manifest.task.as_deref(),
        repo_root: &root,
        allow_secrets: args.allow_secrets,
        redact: args.redact,
    };
    let serialized = if manifest.delivery == PageDelivery::Manifest {
        let selection_started = std::time::Instant::now();
        let selection = select_manifest_page(
            &tower,
            config,
            manifest.task.as_deref(),
            &root,
            args.verbose || config.verbose_output,
        );
        if args.verbose || config.verbose_output {
            eprintln!(
                "page selection: {:?} ({} promoted, signal {})",
                selection_started.elapsed(),
                selection.promoted.len(),
                selection.signal
            );
        }
        for index in &selection.indices {
            let member = &tower.members[*index];
            let file = project
                .file(&member.rel)
                .expect("tower member remains in project graph");
            manifest.files.push(PageFileState {
                rel: member.rel.clone(),
                tier: member.tier,
                base_sha: file.blake3.clone(),
                added_via: PageAddReason::Opened,
                task_relevant: selection.promoted.contains(index),
            });
        }
        serialize_document(
            &[context_meta(&manifest, &tower, &project, Some(&entry_env))],
            &[],
        )?
    } else {
        let mut files = Vec::new();
        let mut states = Vec::new();
        for member in &tower.members {
            if member.tier == Tier::Zero || (member.tier == Tier::One && config.page_tier1_include)
            {
                let source = build_source_file(&root.join(&member.rel))?;
                states.push(PageFileState {
                    rel: member.rel.clone(),
                    tier: member.tier,
                    base_sha: source.base_sha.clone().expect("page source has SHA"),
                    added_via: PageAddReason::Opened,
                    task_relevant: false,
                });
                files.push(source);
            }
        }
        files = crate::secrets::enforce(&files, config, args.allow_secrets, args.redact)?;
        let mut selected = Vec::new();
        let budget = args.max_slop_bytes.unwrap_or(config.max_slop_bytes);
        for (state, source) in states.into_iter().zip(files) {
            if state.tier == Tier::Zero {
                manifest.files.push(state);
                selected.push(source);
                continue;
            }
            manifest.files.push(state);
            selected.push(source);
            let meta = context_meta(&manifest, &tower, &project, None);
            if serialize_document(std::slice::from_ref(&meta), &selected)?.len() > budget {
                manifest.files.pop();
                selected.pop();
            }
        }
        let serialized = serialize_document(
            &[context_meta(&manifest, &tower, &project, None)],
            &selected,
        )?;
        if serialized.len() > budget {
            let _ = fs::remove_dir(page::page_dir(config, &manifest.repo_id, &page_id));
            return Err(SlopError::PageByteBudgetExceeded {
                actual: serialized.len(),
                cap: budget,
            });
        }
        serialized
    };
    fs::create_dir_all(context.parent().expect("context parent")).map_err(|source| {
        SlopError::DirectoryCreationFailure {
            path: context.parent().unwrap().to_path_buf(),
            source,
        }
    })?;
    fs::write(&context, &serialized).map_err(|source| SlopError::FileWriteFailure {
        path: context.clone(),
        source,
    })?;
    page::save_page(config, &manifest)?;
    if manifest.delivery == PageDelivery::Manifest {
        print!("{serialized}");
    } else if !args.silent {
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
    let mut sources = if manifest.delivery == PageDelivery::Bundle {
        document
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
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
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
        if !path.exists() && args.page_add_create {
            let parent = path.parent().expect("page input has parent");
            fs::create_dir_all(parent).map_err(|source| SlopError::DirectoryCreationFailure {
                path: parent.to_path_buf(),
                source,
            })?;
            fs::write(&path, "").map_err(|source| SlopError::FileWriteFailure {
                path: path.clone(),
                source,
            })?;
        }
        let source = build_source_file(&path)?;
        let source = if manifest.delivery == PageDelivery::Bundle {
            let mut checked = crate::secrets::enforce(
                std::slice::from_ref(&source),
                config,
                args.allow_secrets,
                args.redact,
            )?;
            checked
                .pop()
                .expect("one source remains after secret enforcement")
        } else {
            source
        };
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
            task_relevant: false,
        });
        if manifest.delivery == PageDelivery::Bundle {
            sources.push(source);
        }
    }
    let project = graphstore::refresh_project_graph(&root, config, false)?.0;
    let entry_env = if manifest.delivery == PageDelivery::Manifest {
        Some(EntryEnv {
            config,
            task: manifest.task.as_deref(),
            repo_root: &root,
            allow_secrets: args.allow_secrets,
            redact: args.redact,
        })
    } else {
        None
    };
    document
        .meta_blocks
        .retain(|meta| meta.kind != "context-page");
    document.meta_blocks.insert(
        0,
        context_meta(&manifest, &tower, &project, entry_env.as_ref()),
    );
    let serialized = if manifest.delivery == PageDelivery::Manifest {
        serialize_document(&document.meta_blocks, &[])?
    } else {
        serialize_document(&document.meta_blocks, &sources)?
    };
    fs::write(
        page::context_path(config, &repo_id, &manifest.page_id),
        serialized,
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
        .map(|file| canonicalize_path(&root.join(&file.rel)))
        .collect();
    let direct_changes = direct_page_changes(&root, &manifest)?;
    let out_of_scope_changes = direct_out_of_scope_changes(&root, &manifest)?;
    if !out_of_scope_changes.is_empty() {
        if args.strict_page_close {
            return Err(SlopError::PageDirectWritesOutsideScope {
                page: manifest.page_id.clone(),
                paths: out_of_scope_changes,
            });
        }
        if !args.silent {
            eprintln!(
                "warning: page {} has direct edits outside its scope: {}",
                manifest.page_id,
                out_of_scope_changes
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let returned = page::page_dir(config, &repo_id, &manifest.page_id).join("returned");
    let mut documents = Vec::new();
    match fs::read_dir(&returned) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|source| SlopError::FileReadFailure {
                    path: returned.clone(),
                    source,
                })?;
                let content = fs::read_to_string(entry.path()).map_err(|source| {
                    SlopError::FileReadFailure {
                        path: entry.path(),
                        source,
                    }
                })?;
                let document = parse_document(&content)?;
                for block in &document.blocks {
                    let path = canonicalize_path(&block.original_absolute_path);
                    if !allowed.contains(&path) {
                        return Err(SlopError::PageWriteOutsideScope {
                            path,
                            page: manifest.page_id.clone(),
                        });
                    }
                }
                documents.push(document);
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(SlopError::FileReadFailure {
                path: returned,
                source,
            });
        }
    }
    if documents.is_empty() && direct_changes.is_empty() && !args.allow_empty_page_close {
        return Err(SlopError::PageCloseNothingToApply {
            page: manifest.page_id.clone(),
        });
    }
    let direct_change_count = direct_changes.len();
    let mut returned_changes = BTreeSet::new();
    for document in documents {
        returned_changes.extend(
            crate::deslop::apply_document(document, args, config, Some(&allowed))?
                .into_iter()
                .map(|path| canonicalize_path(&path)),
        );
    }
    let (_, report) = graphstore::refresh_project_graph(&root, config, false)?;
    manifest.closed_changes = manifest
        .files
        .iter()
        .filter_map(|file| {
            let path = canonicalize_path(&root.join(&file.rel));
            match (
                direct_changes.contains(&file.rel),
                returned_changes.contains(&path),
            ) {
                (false, false) => None,
                (true, false) => Some(PageCloseSource::Direct),
                (false, true) => Some(PageCloseSource::Returned),
                (true, true) => Some(PageCloseSource::DirectAndReturned),
            }
            .map(|source| PageCloseChange {
                rel: file.rel.clone(),
                source,
            })
        })
        .collect();
    manifest.status = PageStatus::Closed;
    manifest.closed_at_unix = Some(now_unix());
    page::save_page(config, &manifest)?;
    if !args.silent {
        eprintln!(
            "page {} closed; {} direct, {} returned; {}",
            manifest.page_id,
            direct_change_count,
            returned_changes.len(),
            report.summary()
        );
    }
    Ok(())
}

fn direct_page_changes(
    root: &Path,
    manifest: &PageManifest,
) -> Result<BTreeSet<String>, SlopError> {
    manifest
        .files
        .iter()
        .filter_map(|file| {
            let path = root.join(&file.rel);
            match fs::read(&path) {
                Ok(contents) => (blake3::hash(&contents).to_hex().as_str() != file.base_sha)
                    .then_some(Ok(file.rel.clone())),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    Some(Ok(file.rel.clone()))
                }
                Err(source) => Some(Err(SlopError::FileReadFailure { path, source })),
            }
        })
        .collect()
}

fn direct_out_of_scope_changes(
    root: &Path,
    manifest: &PageManifest,
) -> Result<Vec<PathBuf>, SlopError> {
    let scoped: BTreeSet<&str> = manifest
        .files
        .iter()
        .map(|file| file.rel.as_str())
        .collect();
    let base = manifest.base_git_head.as_deref().unwrap_or("HEAD");
    let mut changed = git_paths(root, &["diff", "--name-only", "-z", base])?;
    changed.extend(git_paths(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?);
    Ok(changed
        .into_iter()
        .filter(|rel| !scoped.contains(rel.as_str()))
        .map(|rel| root.join(rel))
        .collect())
}

fn git_head(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|head| !head.is_empty())
}

fn git_paths(root: &Path, args: &[&str]) -> Result<BTreeSet<String>, SlopError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| {
            SlopError::GraphStoreFailure(format!("git {}: {error}", args.join(" ")))
        })?;
    if !output.status.success() {
        return Err(SlopError::GraphStoreFailure(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == b'\0')
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).replace('\\', "/"))
        .collect())
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
    entry_env: Option<&EntryEnv<'_>>,
) -> SoupMetaBlock {
    if manifest.delivery == PageDelivery::Manifest {
        let mut lines = vec![
            "# CONTEXT MANIFEST".to_string(),
            format!(
                "# page: {}   task: {}",
                manifest.page_id,
                manifest.task.as_deref().unwrap_or("(none)")
            ),
            format!("# repo: {}", manifest.repo_root),
            "# delivery: manifest (source is intentionally not preloaded)".to_string(),
            "# Read an entry's anchored region first; widen only if it proves insufficient. Do not read files speculatively.".to_string(),
            "# Batch every known out-of-scope edit into one --page-add path... call. Unregistered edits are reported at close (or rejected by --strict).".to_string(),
            "#".to_string(),
            "# RANKED FILES (page scope):".to_string(),
        ];
        if manifest.files.iter().any(|file| file.task_relevant) {
            lines.push(
                "# * = admitted by task relevance (reserved slots), not score order.".to_string(),
            );
        }
        for state in &manifest.files {
            lines.push(manifest_entry_line(state, tower, project));
            if let Some(env) = entry_env {
                lines.extend(manifest_entry_points(state, env));
            }
        }
        let omitted = tower.members.len().saturating_sub(manifest.files.len());
        lines.push("#".to_string());
        lines.push(format!(
            "# {omitted} lower-ranked tower members omitted; --page-add can extend the page scope when needed."
        ));
        return SoupMetaBlock {
            label: "context-page".to_string(),
            kind: "context-page".to_string(),
            format: "text".to_string(),
            line_count: lines.len(),
            readonly: true,
            content_lines: lines,
        };
    }
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
        "# TIER 1 (outline only - not bundled by page budget or configuration):".to_string(),
    ];
    for member in tower
        .members
        .iter()
        .filter(|member| member.tier == Tier::One && !bundled.contains(member.rel.as_str()))
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
    lines.push("# TIER 2 (outline only - request the file to see full text):".to_string());
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
    lines.extend(["#".to_string(), "# To pull a tier-2/3 file into full text, emit: slop -r \"<absolute path>\" -s".to_string()]);
    SoupMetaBlock {
        label: "context-page".to_string(),
        kind: "context-page".to_string(),
        format: "text".to_string(),
        line_count: lines.len(),
        readonly: true,
        content_lines: lines,
    }
}

/// Values the entry-anchor renderer needs from the CLI/config at render time.
struct EntryEnv<'a> {
    config: &'a Config,
    task: Option<&'a str>,
    repo_root: &'a Path,
    allow_secrets: bool,
    redact: bool,
}

/// Resolve `entry:` lines for one manifest file, as many as the configured
/// per-file cap. None of the existing first-line anchor semantics change.
fn manifest_entry_points(state: &PageFileState, env: &EntryEnv) -> Vec<String> {
    let Some(task) = env.task else {
        return Vec::new();
    };
    if !env.config.page_manifest_entry_anchors {
        return Vec::new();
    }
    let path = env.repo_root.join(&state.rel);
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let scanned = crate::anchor::scan_identifiers(&text);
    let points = crate::anchor::entry_points(
        &scanned,
        task,
        env.config.page_manifest_max_entry_points_per_file,
        env.config.anchor_affinity_min_shared_tokens,
        env.config.anchor_sorted_block_min_entries,
        env.config.anchor_sorted_block_max_line_gap,
    );
    points
        .into_iter()
        .map(|point| {
            let preview = entry_preview(&state.rel, &text, point.line, env);
            match point.insertion {
                Some(ins) => format!(
                    "#     entry: L{line}  {preview}   [insert after L{line}, before L{}; block L{}-L{}]",
                    ins.before_line,
                    ins.span_lo,
                    ins.span_hi,
                    line = point.line,
                    preview = preview,
                ),
                None => format!(
                    "#     entry: L{line}  {preview}",
                    line = point.line,
                    preview = preview
                ),
            }
        })
        .collect()
}

/// The raw preview of one file line, truncated to the configured width and
/// routed through the same secrets enforcement as the bundle path. A finding
/// suppresses the text but keeps the line number; `--allow-secrets` and
/// `--redact` behave exactly as they do for the bundle path.
fn entry_preview(rel: &str, text: &str, line: usize, env: &EntryEnv) -> String {
    let raw = text
        .lines()
        .nth(line.saturating_sub(1))
        .unwrap_or("")
        .trim()
        .to_string();
    let raw = if raw.chars().count() > env.config.page_manifest_entry_preview_chars {
        let mut truncated: String = raw
            .chars()
            .take(env.config.page_manifest_entry_preview_chars)
            .collect();
        truncated.push('…');
        truncated
    } else {
        raw
    };
    let source = crate::models::SourceFile {
        original_absolute_path: PathBuf::from(rel),
        file_name: rel.to_string(),
        name_token: rel.to_string(),
        contents: raw.trim_end().to_string(),
        logical_line_count: 1,
        trailing_newline: false,
        base_sha: None,
        read_only: false,
    };
    let files = [source];
    let on_mode = {
        let mode = env.config.secret_scan.trim().to_lowercase();
        !matches!(mode.as_str(), "off" | "disabled" | "false" | "none")
    };
    if !on_mode {
        return files[0].contents.clone();
    }
    let findings = crate::secrets::scan_files(&files);
    for finding in &findings {
        eprintln!(
            "warning: entry preview skipped a secrets finding: {rel}:{} {}",
            finding.line, finding.rule
        );
    }
    if findings.is_empty() {
        return files[0].contents.clone();
    }
    if env.allow_secrets {
        return files[0].contents.clone();
    }
    if env.redact {
        let mut files_mut = files.to_vec();
        crate::secrets::apply_redaction(&mut files_mut, &findings);
        return files_mut[0].contents.trim_end().to_string();
    }
    format!(
        "(suppressed: {} secrets finding)",
        findings
            .first()
            .map(|f| f.rule.as_str())
            .unwrap_or("possible")
    )
}

fn manifest_entry_line(
    state: &PageFileState,
    tower: &graphstore::TowerGraph,
    project: &graphstore::ProjectGraph,
) -> String {
    let member = tower.members.iter().find(|member| member.rel == state.rel);
    let via = member
        .map(|member| member.via.as_slice())
        .unwrap_or_default();
    let mut idents = BTreeSet::new();
    for edge in &project.symbol_edges {
        let connects_via = via.iter().any(|via| {
            (edge.from == state.rel && edge.to == *via)
                || (edge.to == state.rel && edge.from == *via)
        });
        if connects_via {
            idents.extend(edge.idents.iter().cloned());
        }
    }
    let mut anchors = Vec::new();
    if let Some(file) = project.file(&state.rel) {
        // A shared identifier is usually DEFINED in the seed and REFERENCED
        // here, so the useful anchor is the reference site. Prefer those, then
        // fall back to definitions.
        for want_def in [false, true] {
            for tag in file.tags.iter().filter(|tag| tag.def == want_def) {
                if !idents.is_empty() && !idents.contains(&tag.name) {
                    continue;
                }
                if idents.is_empty() && !tag.def {
                    continue;
                }
                if !anchors.contains(&tag.line) {
                    anchors.push(tag.line);
                }
                if anchors.len() == 3 {
                    break;
                }
            }
            if anchors.len() == 3 {
                break;
            }
        }
    }
    let via = if via.is_empty() {
        "seed".to_string()
    } else {
        via.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
    };
    let shares = if idents.is_empty() {
        "(co-change or no symbol)".to_string()
    } else {
        idents.into_iter().take(3).collect::<Vec<_>>().join(", ")
    };
    let anchors = if anchors.is_empty() {
        "(no anchor)".to_string()
    } else {
        anchors
            .into_iter()
            .map(|line| format!("L{line}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let tier = if state.task_relevant {
        format!("{}*", tier_label(state.tier))
    } else {
        tier_label(state.tier).to_string()
    };
    format!(
        "#   {}   {}   via: {}   shares: {}   anchor: {}",
        state.rel, tier, via, shares, anchors
    )
}

fn tier_label(tier: Tier) -> &'static str {
    match tier {
        Tier::Zero => "tier-0",
        Tier::One => "tier-1",
        Tier::Two => "tier-2",
        Tier::Three => "tier-3",
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

/// Allocate the page directory as part of id creation. The exclusive directory
/// creation makes concurrent opens with identical seeds unable to overwrite one
/// another, while the millisecond prefix keeps page ids chronologically sorted.
fn allocate_page_id(
    config: &Config,
    repo_id: &str,
    root: &std::path::Path,
    seeds: &[String],
) -> Result<String, SlopError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let suffix = &blake3::hash(format!("{}:{seeds:?}", root.display()).as_bytes()).to_hex()[..8];
    let parent = page::pages_dir(config).join(repo_id);
    fs::create_dir_all(&parent).map_err(|source| SlopError::DirectoryCreationFailure {
        path: parent.clone(),
        source,
    })?;
    for sequence in 0_u64.. {
        let page_id = if sequence == 0 {
            format!("{millis:013x}-{suffix}")
        } else {
            format!("{millis:013x}-{suffix}-{sequence}")
        };
        let path = parent.join(&page_id);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(page_id),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(SlopError::DirectoryCreationFailure { path, source }),
        }
    }
    unreachable!("unbounded page id sequence")
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
    use crate::graphstore::model::{FileEntry, GraphStats, StoredTag, Structure, SymbolEdge};
    use crate::graphstore::{TowerGraph, TowerMember};

    fn manifest() -> PageManifest {
        PageManifest {
            schema: PAGE_SCHEMA,
            page_id: "001-test".to_string(),
            repo_id: "repo".to_string(),
            repo_root: "/repo".to_string(),
            task: None,
            base_git_head: None,
            status: PageStatus::Open,
            seed_digest: "digest".to_string(),
            opened_at_unix: 0,
            closed_at_unix: None,
            delivery: PageDelivery::Bundle,
            files: vec![PageFileState {
                rel: "a.rs".to_string(),
                tier: Tier::Zero,
                base_sha: "0".repeat(64),
                added_via: PageAddReason::Opened,
                task_relevant: false,
            }],
            closed_changes: Vec::new(),
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
            ranking_fingerprint: crate::graphstore::ranking_fingerprint(&Config::default()),
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
            community_couplings: Vec::new(),
            directory_couplings: Vec::new(),
            structure: Default::default(),
            stats: Default::default(),
        };
        let meta = context_meta(&manifest(), &tower, &project, None);
        assert!(meta.readonly);
        assert!(meta.content_lines.iter().any(|line| line.contains("b.rs")));
        assert!(meta.content_lines.iter().any(|line| line.contains("c.rs")));
    }

    #[test]
    fn manifest_metadata_anchors_a_candidate_reference_to_a_seed_symbol() {
        let mut manifest = manifest();
        manifest.delivery = PageDelivery::Manifest;
        manifest.files.push(PageFileState {
            rel: "b.rs".to_string(),
            tier: Tier::One,
            base_sha: "1".repeat(64),
            added_via: PageAddReason::Opened,
            task_relevant: false,
        });
        let tower = TowerGraph {
            schema: 2,
            repo_id: "repo".to_string(),
            seed_digest: "digest".to_string(),
            seeds: vec!["a.rs".to_string()],
            project_graph_fingerprint: "fingerprint".to_string(),
            ranking_fingerprint: "ranking".to_string(),
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
                    tier: Tier::One,
                    score: 0.5,
                    via: vec!["a.rs".to_string()],
                },
            ],
            cut_scores: [0.0; 3],
        };
        let file = |rel: &str, tags| FileEntry {
            rel: rel.to_string(),
            blake3: "0".repeat(64),
            size: 1,
            parsed: true,
            tags,
            rank: 0.0,
            afferent: 0,
            efferent: 0,
            instability: 1.0,
            def_count: 0,
            community: None,
        };
        let project = crate::graphstore::ProjectGraph {
            schema: 3,
            repo_root: "/repo".to_string(),
            repo_id: "repo".to_string(),
            generated_at_unix: 0,
            generator_version: "test".to_string(),
            files: vec![
                file("a.rs", vec![]),
                file(
                    "b.rs",
                    vec![StoredTag {
                        name: "shared".to_string(),
                        line: 2,
                        def: false,
                    }],
                ),
            ],
            symbol_edges: vec![SymbolEdge {
                from: "b.rs".to_string(),
                to: "a.rs".to_string(),
                weight: 1.0,
                idents: vec!["shared".to_string()],
            }],
            cochange_edges: vec![],
            communities: vec![],
            community_couplings: vec![],
            directory_couplings: vec![],
            structure: Structure::default(),
            stats: GraphStats::default(),
        };
        let rendered = context_meta(&manifest, &tower, &project, None)
            .content_lines
            .join("\n");
        assert!(rendered.contains("via: a.rs"), "{rendered}");
        assert!(rendered.contains("shares: shared"), "{rendered}");
        assert!(rendered.contains("anchor: L2"), "{rendered}");
    }

    #[test]
    fn accepts_only_documented_prune_durations() {
        assert_eq!(parse_duration("7d").expect("days"), 604_800);
        assert_eq!(parse_duration("30m").expect("minutes"), 1_800);
        assert!(parse_duration("forever").is_err());
    }

    fn write_rel_file(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(path, contents).expect("write fixture");
    }

    fn env_for<'a>(config: &'a Config, task: Option<&'a str>, root: &'a Path) -> EntryEnv<'a> {
        EntryEnv {
            config,
            task,
            repo_root: root,
            allow_secrets: false,
            redact: false,
        }
    }

    #[test]
    fn unsorted_logic_file_yields_no_entry_lines_and_keeps_reference_anchor() {
        let config = Config::default();
        let dir = tempfile::tempdir().expect("tempdir");
        write_rel_file(
            dir.path(),
            "logic.rs",
            "def parse_config (): 1
render_page_now ()
compute_hash ()
def validate_input(): 1
",
        );
        let mut manifest = manifest();
        manifest.delivery = PageDelivery::Manifest;
        manifest.task = Some("implement render-blue-fixture".to_string());
        let tower = TowerGraph {
            schema: 2,
            repo_id: "repo".to_string(),
            seed_digest: "digest".to_string(),
            seeds: vec!["a.rs".to_string()],
            project_graph_fingerprint: "fingerprint".to_string(),
            ranking_fingerprint: "ranking".to_string(),
            generated_at_unix: 0,
            members: vec![TowerMember {
                rel: "a.rs".to_string(),
                tier: Tier::Zero,
                score: 1.0,
                via: Vec::new(),
            }],
            cut_scores: [0.0; 3],
        };
        let file = |rel: &str, tags| FileEntry {
            rel: rel.to_string(),
            blake3: "0".repeat(64),
            size: 1,
            parsed: true,
            tags,
            rank: 0.0,
            afferent: 0,
            efferent: 0,
            instability: 1.0,
            def_count: 0,
            community: None,
        };
        let project = crate::graphstore::ProjectGraph {
            schema: 3,
            repo_root: "/repo".to_string(),
            repo_id: "repo".to_string(),
            generated_at_unix: 0,
            generator_version: "test".to_string(),
            files: vec![
                file(
                    "a.rs",
                    vec![StoredTag {
                        name: "shared".to_string(),
                        line: 2,
                        def: false,
                    }],
                ),
                file("logic.rs", vec![]),
            ],
            symbol_edges: vec![SymbolEdge {
                from: "a.rs".to_string(),
                to: "logic.rs".to_string(),
                weight: 1.0,
                idents: vec!["shared".to_string()],
            }],
            cochange_edges: vec![],
            communities: vec![],
            community_couplings: vec![],
            directory_couplings: vec![],
            structure: Structure::default(),
            stats: GraphStats::default(),
        };
        let env = env_for(&config, manifest.task.as_deref(), dir.path());
        manifest.files.push(PageFileState {
            rel: "logic.rs".to_string(),
            tier: Tier::One,
            base_sha: "1".repeat(64),
            added_via: PageAddReason::Opened,
            task_relevant: false,
        });
        let rendered = context_meta(&manifest, &tower, &project, Some(&env))
            .content_lines
            .join("\n");
        assert!(!rendered.contains("entry:"), "{rendered}");
        assert!(rendered.contains("logic.rs"), "{rendered}");
        assert!(rendered.contains("anchor:"), "{rendered}");
    }

    #[test]
    fn no_task_yields_byte_identical_manifest_output() {
        let config = Config::default();
        let dir = tempfile::tempdir().expect("tempdir");
        write_rel_file(dir.path(), "a.rs", "pub fn shared() {}\n");
        let mut manifest = manifest();
        manifest.delivery = PageDelivery::Manifest;
        let tower = TowerGraph {
            schema: 2,
            repo_id: "repo".to_string(),
            seed_digest: "digest".to_string(),
            seeds: vec!["a.rs".to_string()],
            project_graph_fingerprint: "fingerprint".to_string(),
            ranking_fingerprint: "ranking".to_string(),
            generated_at_unix: 0,
            members: vec![TowerMember {
                rel: "a.rs".to_string(),
                tier: Tier::Zero,
                score: 1.0,
                via: Vec::new(),
            }],
            cut_scores: [0.0; 3],
        };
        let project = crate::graphstore::ProjectGraph {
            schema: 3,
            repo_root: "/repo".to_string(),
            repo_id: "repo".to_string(),
            generated_at_unix: 0,
            generator_version: "test".to_string(),
            files: Vec::new(),
            symbol_edges: Vec::new(),
            cochange_edges: Vec::new(),
            communities: Vec::new(),
            community_couplings: Vec::new(),
            directory_couplings: Vec::new(),
            structure: Structure::default(),
            stats: GraphStats::default(),
        };
        let without_env = context_meta(&manifest, &tower, &project, None);
        let env = env_for(&config, None, dir.path());
        let with_env = context_meta(&manifest, &tower, &project, Some(&env));
        assert_eq!(
            without_env.content_lines, with_env.content_lines,
            "task-less pages must render byte-identically with and without the anchor env"
        );
    }

    #[test]
    fn secret_in_preview_suppresses_text_but_keeps_line_number() {
        let config = Config::default();
        let dir = tempfile::tempdir().expect("tempdir");
        write_rel_file(
            dir.path(),
            "reg.rs",
            "alpha_secret_audit = \"AKIA1234567890123456\"\ngamma_plain_utility = 1\n",
        );
        let env = env_for(&config, Some("alpha-secret-audit"), dir.path());
        let state = PageFileState {
            rel: "reg.rs".to_string(),
            tier: Tier::One,
            base_sha: "2".repeat(64),
            added_via: PageAddReason::Opened,
            task_relevant: false,
        };
        let lines = manifest_entry_points(&state, &env);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("entry: L1"), "{lines:?}");
        assert!(lines[0].contains("suppressed"), "{lines:?}");
        assert!(!lines[0].contains("AKIA1234567890123456"), "{lines:?}");
    }

    #[test]
    fn allow_secrets_shows_preview_despite_finding() {
        let config = Config::default();
        let dir = tempfile::tempdir().expect("tempdir");
        write_rel_file(
            dir.path(),
            "reg.rs",
            "alpha_secret_audit = \"AKIA1234567890123456\"\ngamma_plain_utility = 1\n",
        );
        let mut env = env_for(&config, Some("alpha-secret-audit"), dir.path());
        env.allow_secrets = true;
        let state = PageFileState {
            rel: "reg.rs".to_string(),
            tier: Tier::One,
            base_sha: "3".repeat(64),
            added_via: PageAddReason::Opened,
            task_relevant: false,
        };
        let lines = manifest_entry_points(&state, &env);
        assert!(lines[0].contains("AKIA1234567890123456"), "{lines:?}");
    }

    fn selection_tower() -> TowerGraph {
        let member = |rel: &str, tier, score| TowerMember {
            rel: rel.to_string(),
            tier,
            score,
            via: Vec::new(),
        };
        TowerGraph {
            schema: 2,
            repo_id: "repo".to_string(),
            seed_digest: "digest".to_string(),
            seeds: vec!["a.rs".to_string()],
            project_graph_fingerprint: "fingerprint".to_string(),
            ranking_fingerprint: "ranking".to_string(),
            generated_at_unix: 0,
            members: vec![
                member("a.rs", Tier::Zero, 1.0),
                member("b.rs", Tier::One, 0.5),
                member("c.rs", Tier::Three, 0.01),
                member("d.rs", Tier::Three, 0.005),
            ],
            cut_scores: [0.0; 3],
        }
    }

    #[test]
    fn task_relevance_promotion_reserves_a_slot_for_a_qualifier_file() {
        let config = Config {
            page_manifest_max_files: 2,
            page_task_relevance_reserved_slots: 1,
            ..Config::default()
        };
        let dir = tempfile::tempdir().expect("tempdir");
        write_rel_file(dir.path(), "c.rs", "use flake8_pyi::rules;\n");
        let selection = select_manifest_page(
            &selection_tower(),
            &config,
            Some("[`flake8-pyi`] Implement `redundant-none-literal` (`PYI061`)"),
            dir.path(),
            false,
        );
        assert_eq!(selection.indices, vec![0, 2], "seed then promoted");
        assert!(selection.promoted.contains(&2));
    }

    #[test]
    fn higher_selection_score_beats_earlier_tower_order() {
        let config = Config {
            page_manifest_max_files: 2,
            page_task_relevance_reserved_slots: 1,
            ..Config::default()
        };
        let dir = tempfile::tempdir().expect("tempdir");
        write_rel_file(dir.path(), "c.rs", "use flake8_pyi::rules;\n");
        write_rel_file(
            dir.path(),
            "d.rs",
            "use flake8_pyi;\nuse flake8_pyi;\nuse flake8_pyi;\n",
        );
        let selection = select_manifest_page(
            &selection_tower(),
            &config,
            Some("[`flake8-pyi`] Implement `redundant-none-literal` (`PYI061`)"),
            dir.path(),
            false,
        );
        assert_eq!(
            selection.indices,
            vec![0, 3],
            "the higher-scoring d.rs must win the one slot despite ranking later"
        );
        assert!(selection.promoted.contains(&3));
    }

    #[test]
    fn registry_hint_scores_a_plugin_mod_rs_from_its_path() {
        let config = Config {
            page_manifest_max_files: 2,
            page_task_relevance_reserved_slots: 1,
            ..Config::default()
        };
        let dir = tempfile::tempdir().expect("tempdir");
        write_rel_file(
            dir.path(),
            "flake8_pyi/rules/mod.rs",
            "mod alpha;\nmod beta;\n",
        );
        let tower = {
            let mut tower = selection_tower();
            tower.members[2].rel = "flake8_pyi/rules/mod.rs".to_string();
            tower
        };
        let selection = select_manifest_page(
            &tower,
            &config,
            Some("[`flake8-pyi`] Implement `redundant-none-literal` (`PYI061`)"),
            dir.path(),
            false,
        );
        assert!(
            selection.promoted.contains(&2),
            "a plugin mod.rs scores via its path even with no content match: {selection:?}"
        );
    }

    #[test]
    fn task_relevance_flag_off_matches_legacy_selection() {
        let config = Config {
            page_manifest_max_files: 2,
            page_task_relevance_promotion: false,
            ..Config::default()
        };
        let dir = tempfile::tempdir().expect("tempdir");
        write_rel_file(dir.path(), "c.rs", "use flake8_pyi::rules;\n");
        let selection = select_manifest_page(
            &selection_tower(),
            &config,
            Some("[`flake8-pyi`] Implement `redundant-none-literal` (`PYI061`)"),
            dir.path(),
            false,
        );
        assert_eq!(selection.indices, vec![0, 1]);
        assert!(selection.promoted.is_empty());
    }

    #[test]
    fn no_task_matches_legacy_selection() {
        let config = Config {
            page_manifest_max_files: 2,
            ..Config::default()
        };
        let dir = tempfile::tempdir().expect("tempdir");
        let selection = select_manifest_page(&selection_tower(), &config, None, dir.path(), false);
        assert_eq!(selection.indices, vec![0, 1]);
        assert!(selection.promoted.is_empty());
    }

    fn page_test_git(root: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    fn page_test_commit(root: &Path, message: &str) {
        page_test_git(root, &["add", "-A"]);
        page_test_git(root, &["commit", "-m", message]);
    }

    #[test]
    fn history_selection_promotes_files_prior_additions_touched() {
        let config = Config {
            page_manifest_max_files: 2,
            page_task_relevance_reserved_slots: 1,
            ..Config::default()
        };
        let dir = tempfile::tempdir().expect("tempdir");
        page_test_git(dir.path(), &["init", "-q"]);
        write_rel_file(dir.path(), "a.rs", "seed\n");
        write_rel_file(dir.path(), "c.rs", "v1\n");
        page_test_commit(dir.path(), "add seed");
        write_rel_file(dir.path(), "sibling_b.rs", "b\n");
        write_rel_file(dir.path(), "c.rs", "v2\n");
        page_test_commit(dir.path(), "add sibling b");
        write_rel_file(dir.path(), "sibling_c.rs", "c\n");
        write_rel_file(dir.path(), "c.rs", "v3\n");
        page_test_commit(dir.path(), "add sibling c");

        let selection = select_manifest_page(
            &selection_tower(),
            &config,
            Some("[`flake8-pyi`] Implement `redundant-none-literal` (`PYI061`)"),
            dir.path(),
            false,
        );
        assert_eq!(
            selection.indices,
            vec![0, 2],
            "c.rs was touched by 2 of 3 prior sibling additions and must win the slot"
        );
        assert!(selection.promoted.contains(&2));
    }

    #[test]
    fn task_relevant_manifest_line_is_marked_with_a_star_and_legend() {
        let mut manifest = manifest();
        manifest.delivery = PageDelivery::Manifest;
        manifest.files = vec![PageFileState {
            rel: "c.rs".to_string(),
            tier: Tier::Three,
            base_sha: "3".repeat(64),
            added_via: PageAddReason::Opened,
            task_relevant: true,
        }];
        let tower = TowerGraph {
            schema: 2,
            repo_id: "repo".to_string(),
            seed_digest: "digest".to_string(),
            seeds: vec!["c.rs".to_string()],
            project_graph_fingerprint: "fingerprint".to_string(),
            ranking_fingerprint: "ranking".to_string(),
            generated_at_unix: 0,
            members: vec![TowerMember {
                rel: "c.rs".to_string(),
                tier: Tier::Zero,
                score: 1.0,
                via: Vec::new(),
            }],
            cut_scores: [0.0; 3],
        };
        let project = crate::graphstore::ProjectGraph {
            schema: 3,
            repo_root: "/repo".to_string(),
            repo_id: "repo".to_string(),
            generated_at_unix: 0,
            generator_version: "test".to_string(),
            files: Vec::new(),
            symbol_edges: Vec::new(),
            cochange_edges: Vec::new(),
            communities: Vec::new(),
            community_couplings: Vec::new(),
            directory_couplings: Vec::new(),
            structure: Structure::default(),
            stats: GraphStats::default(),
        };
        let rendered = context_meta(&manifest, &tower, &project, None)
            .content_lines
            .join("\n");
        assert!(rendered.contains("tier-3*"), "{rendered}");
        assert!(
            rendered.contains("admitted by task relevance"),
            "{rendered}"
        );
    }
}
