//! The on-disk shape of a persisted graph.
//!
//! Everything here is `serde`-round-trippable and deliberately flat: the store
//! is a cache, and a cache that needs a migration layer is a database wearing a
//! disguise. When a field changes meaning, bump [`PROJECT_GRAPH_SCHEMA`] and the
//! loader discards the old file instead of migrating it. A full rebuild of a
//! mid-size repo costs seconds; carrying migration code forever costs more.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::repomap::tags::{Tag, TagKind};

/// Bump on any incompatible change to the structures below.
pub const PROJECT_GRAPH_SCHEMA: u32 = 1;

/// One definition or reference, cached so an unchanged file is never re-parsed.
///
/// This is the whole point of persisting the graph. Tree-sitter parsing is the
/// expensive step; ranking, community detection and rendering are cheap in
/// comparison and are always redone from the full tag set, which keeps the
/// incremental path from drifting away from a cold build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTag {
    pub name: String,
    pub line: usize,
    /// `true` for a definition, `false` for a reference.
    pub def: bool,
}

impl StoredTag {
    pub fn to_tag(&self, rel_fname: &str) -> Tag {
        Tag {
            rel_fname: rel_fname.to_string(),
            line: self.line,
            name: self.name.clone(),
            kind: if self.def { TagKind::Def } else { TagKind::Ref },
        }
    }

    pub fn from_tag(tag: &Tag) -> Self {
        Self {
            name: tag.name.clone(),
            line: tag.line,
            def: tag.kind == TagKind::Def,
        }
    }
}

/// A file as the graph sees it: identity, cached parse, and its derived numbers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    /// Repo-relative path, forward-slashed, the graph's primary key.
    pub rel: String,
    /// blake3 of the file's bytes at build time. The incremental key.
    pub blake3: String,
    pub size: u64,
    /// Whether a tree-sitter grammar claimed this file. `false` means the file
    /// exists in the manifest but contributes no edges.
    pub parsed: bool,
    pub tags: Vec<StoredTag>,
    /// Personalized PageRank over the dependency graph.
    pub rank: f64,
    /// Ca: distinct files referencing something defined here.
    pub afferent: usize,
    /// Ce: distinct files this one reaches into.
    pub efferent: usize,
    /// Ce / (Ca + Ce). Near 0 = depended upon, risky to change.
    pub instability: f64,
    pub def_count: usize,
    /// Index into [`ProjectGraph::communities`], if the file landed in one.
    pub community: Option<usize>,
}

impl FileEntry {
    pub fn risk(&self) -> &'static str {
        match self.afferent {
            0 => "none",
            1..=2 => "low",
            3..=7 => "medium",
            _ => "high",
        }
    }

    pub fn def_tags(&self) -> impl Iterator<Item = &StoredTag> {
        self.tags.iter().filter(|tag| tag.def)
    }
}

/// A directed file-to-file dependency justified by shared identifiers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEdge {
    pub from: String,
    pub to: String,
    pub weight: f64,
    /// The identifiers carrying the most weight on this edge, for display.
    pub idents: Vec<String>,
}

/// An undirected "these change together" edge mined from git history.
///
/// Co-change catches coupling the symbol graph cannot see: a Rust struct and
/// the SQL migration that shapes it, a component and its snapshot test, a
/// config key and the code reading it by string. `a` is always < `b`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoChangeEdge {
    pub a: String,
    pub b: String,
    /// How many commits touched both files.
    pub commits: usize,
    /// Commit count discounted by commit breadth, so a 30-file sweep does not
    /// out-vote a focused two-file change.
    pub weight: f64,
}

/// A cluster of files that behave as a module, whatever the directory tree says.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Community {
    pub id: usize,
    /// Derived from the members' common directory, or their most frequent one.
    pub label: String,
    pub members: Vec<String>,
    /// Summed weight of edges inside the community.
    pub internal_weight: f64,
    /// Summed weight of edges leaving it. High external weight on a small
    /// community usually means the split is wrong or the module is leaking.
    pub external_weight: f64,
}

/// Whole-graph shape: the parts that describe risk rather than content.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Structure {
    /// Strongly connected components of size >= 2: mutual dependency knots.
    pub cycles: Vec<Vec<String>>,
    /// Articulation points; removing one disconnects the graph.
    pub chokepoints: Vec<String>,
    /// Files nothing references.
    pub orphans: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GraphStats {
    pub file_count: usize,
    pub parsed_count: usize,
    pub symbol_edge_count: usize,
    pub cochange_edge_count: usize,
    pub community_count: usize,
    /// Files the manifest walker dropped at its ceiling.
    pub manifest_truncated: usize,
    /// Whether the manifest came from `git ls-files` rather than a raw walk.
    pub from_git: bool,
    /// Modularity of the community split. Roughly: above 0.3 the clustering is
    /// telling you something, near 0 the repo has no module structure the edges
    /// can see and community-based tiering will not help much.
    pub modularity: f64,
}

/// The persisted project graph: one repository, fully described.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectGraph {
    pub schema: u32,
    /// Absolute path at build time. Informational; the store is keyed by hash.
    pub repo_root: String,
    pub repo_id: String,
    pub generated_at_unix: u64,
    pub generator_version: String,
    pub files: Vec<FileEntry>,
    pub symbol_edges: Vec<SymbolEdge>,
    pub cochange_edges: Vec<CoChangeEdge>,
    pub communities: Vec<Community>,
    pub structure: Structure,
    pub stats: GraphStats,
}

impl ProjectGraph {
    /// Stable identity of the inputs used by downstream derived graphs.
    pub fn fingerprint(&self) -> String {
        let encoded = serde_json::to_vec(&(&self.files, &self.symbol_edges, &self.cochange_edges))
            .expect("project graph fields are serializable");
        blake3::hash(&encoded).to_hex().to_string()
    }

    /// Index files by path once, for callers doing repeated lookups.
    pub fn by_rel(&self) -> BTreeMap<&str, &FileEntry> {
        self.files
            .iter()
            .map(|file| (file.rel.as_str(), file))
            .collect()
    }

    pub fn file(&self, rel: &str) -> Option<&FileEntry> {
        self.files
            .binary_search_by(|probe| probe.rel.as_str().cmp(rel))
            .ok()
            .map(|index| &self.files[index])
    }

    /// Rebuild the flat tag list the ranking pass consumes.
    pub fn all_tags(&self) -> Vec<Tag> {
        let mut tags = Vec::new();
        for file in &self.files {
            for stored in &file.tags {
                tags.push(stored.to_tag(&file.rel));
            }
        }
        tags
    }

    /// Undirected symbol-graph neighbours of `rel`, in both directions.
    ///
    /// Stage 2's tower walk starts here: relevance does not care whether a file
    /// calls or is called, only that the two are adjacent.
    pub fn neighbors(&self, rel: &str) -> BTreeSet<&str> {
        let mut out = BTreeSet::new();
        for edge in &self.symbol_edges {
            if edge.from == rel {
                out.insert(edge.to.as_str());
            } else if edge.to == rel {
                out.insert(edge.from.as_str());
            }
        }
        for edge in &self.cochange_edges {
            if edge.a == rel {
                out.insert(edge.b.as_str());
            } else if edge.b == rel {
                out.insert(edge.a.as_str());
            }
        }
        out
    }

    pub fn community(&self, id: usize) -> Option<&Community> {
        self.communities.get(id)
    }
}

/// What a build actually did, so the command can say something honest.
#[derive(Debug, Clone, Default)]
pub struct BuildReport {
    /// Files re-parsed because their hash changed or they were new.
    pub reparsed: usize,
    /// Files whose cached tags were reused verbatim.
    pub reused: usize,
    /// Files present in the previous graph and gone now.
    pub removed: usize,
    /// Commits scanned for co-change signal.
    pub commits_scanned: usize,
    pub elapsed_ms: u128,
    /// True when no previous graph was usable and everything was parsed cold.
    pub cold: bool,
}

impl BuildReport {
    pub fn summary(&self) -> String {
        if self.cold {
            format!(
                "built cold: {} file(s) parsed in {}ms",
                self.reparsed, self.elapsed_ms
            )
        } else {
            format!(
                "refreshed: {} re-parsed, {} reused, {} removed in {}ms",
                self.reparsed, self.reused, self.removed, self.elapsed_ms
            )
        }
    }
}
