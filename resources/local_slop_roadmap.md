# Local slop: context paging

Slopify's existing job is a bridge to remote agents: bundle files, ship them
out, deslop what comes back. Local slop keeps the bundling and drops the trip.
The agent is already on this machine; what it lacks is not access to the files
but a reason to stop reading all of them.

A context page is that reason. It is a bounded, tiered working set built from
the graph: the files the task is actually about, the files those reach into,
and a fading margin of everything else. The agent abandons the full repository
for the page, works inside it, and deslops back when the task is done.

Four stages get us there. Stage 1 is in this bundle.

---

## Stage 0 — where we started

- `-g/--include-graph` computes a repomap and staples it into a slop as a
  `#SLOP_META` block.
- The graph is recomputed from scratch on every invocation and discarded when
  the process exits.
- `--match/--seed/--symbol/--task` select files via BM25 (tantivy), one-hop
  BFS over ident-matched file edges, and a byte budget.
- Everything the graph knows is thrown away between runs.

That was affordable when the graph garnished a bundle. It stops being
affordable the moment the graph decides what an agent is allowed to look at,
because then it is consulted many times per task instead of once per bundle.

---

## Stage 1 — the project graph *(this bundle)*

The graph becomes a persisted artifact with its own command.

```
slop someDir someFile --project-graph
```

Reads the paths only to name repositories. Builds one graph per distinct
repository containing them. Bundles nothing.

**What is in the graph**

| Layer | Source | What it catches |
|---|---|---|
| Symbol edges | tree-sitter tags, ident-matched, ambiguity-damped | proven references |
| Metrics | personalized PageRank, Ca/Ce, instability, risk | what is load-bearing |
| Structure | SCCs, articulation points, orphans | knots and chokepoints |
| Co-change | `git log --name-only`, breadth-discounted | coupling no parser can see |
| Modules | Louvain over symbol + co-change edges | boundaries the tree lies about |

Co-change is the addition that pays for itself: a struct and the migration
that shapes it, a component and its snapshot fixture, a flag and the three
sites reading it by string. The symbol graph is blind to all of it. Two guards
keep it honest — commits touching more than `graph_cochange_max_files_per_commit`
files are dropped as sweeps, and each surviving commit's contribution is
divided by its own breadth.

**Incremental rebuild.** Every file carries the blake3 of its bytes. On
refresh, only files whose hash moved are re-parsed. Ranking, clustering and
structure analysis always run over the *complete* tag set, even incrementally.
That is deliberate and it is the one invariant worth defending: an
incrementally-maintained PageRank drifts away from what a rebuild produces, and
a cache that can disagree with a rebuild is worse than no cache, because every
downstream decision inherits the disagreement silently. Parsing is the
expensive step; it is the only step we skip.

**Storage.**

- Canonical: `$HOME/.cache/slop/graphs/<repo-id>/project.json`, written
  atomically via temp-and-rename. Cache, not state — safe to delete, and it
  sits beside the selection index for that reason.
- Readable: `<slopified>/<repo-id>.project-graph.md`, same dialect as the `-g`
  block, including the `#SLOP_REQUEST` convention. Disable with
  `graph_emit_artifact: false`.
- `<repo-id>` is `<dirname>-<blake3(canonical path)[..16]>`, so two checkouts
  named `api` never collide.

**Schema drift** is handled by not handling it: a stored graph whose `schema`
differs is discarded and rebuilt. Migration code for a regenerable cache costs
more than the rebuild it saves.

**Flags.** `--project-graph` rejects every bundle-shaping flag rather than
ignoring it — `--match`, `--seed`, `-x`, `-r`, `-g` and friends. The dangerous
failure is the silent one: someone passing `--match` and believing the graph
was scoped by it. `--reindex` forces a cold rebuild; it already means "rebuild
the derived index you keep for this repo", and the graph is one of those.

**New config keys:** `graph_store_dir`, `graph_max_files`,
`graph_community_resolution`, `graph_cochange_weight`, `graph_cochange_commits`,
`graph_cochange_max_files_per_commit`, `graph_cochange_min_commits`,
`graph_cochange_max_edges`, `graph_emit_artifact`.

---

## Stage 2 — the tower graph

```
slop file1 file2 someDir --tower-graph
slop someDir --tower-graph -r
```

Tier 0 is what you named: the listed files, plus a directory's immediate
children, or all its descendants under `-r`. Everything else earns its place.

**Why not hops.** BFS depth is a step function. It cannot tell a file called
forty times from one called once, and it has no way to say that two hops
through the same module is nearer than one hop through a utility everything
touches. Random walk with restart handles both, because it is weight-aware and
it dilutes through high-degree nodes instead of sprinting through them.

**The walk.** Personalize on tier 0, restart probability α ≈ 0.2, ~30
iterations, deterministic. Edge weight is symbol weight plus
`cochange_weight × co-change weight`, then damped by `1/sqrt(degree(target))`
so hub files stop acting as highways. A same-community bonus applies: Stage 1's
Louvain labels are exactly the "is this neighbour really in my neighbourhood"
signal that hop counting lacks.

**Tiers** are cut from the score distribution, not from fixed counts, so a
tightly-coupled seed set produces a small page and a diffuse one produces a
wide shallow page:

| Tier | Cut | Treatment in a page |
|---|---|---|
| 0 | named | full text, writable |
| 1 | to 60% of non-seed mass | full text if budget allows, else outline |
| 2 | next 25% | symbol outline only |
| 3 | remainder above ε | manifest line only |
| — | below ε | absent |

Stored at `$HOME/.cache/slop/graphs/<repo-id>/towers/<seed-digest>.json`, keyed
by the seed set so an identical request is free. Emitted as
`<repo-id>.tower-graph.md`.

**Also in scope:** `--tower-graph` rejects the same bundle-shaping flags, minus
`-r`, which it genuinely uses.

---

## Stage 3 — context pages

The paging lifecycle, as flags in the same exclusive-mode style:

```
slop <paths> --page-open  --task "..."   # tower graph -> workspace -> slop
slop --page-add <paths>                  # amend in flight
slop --page-close                        # deslop back, refresh graph
slop --page-list
```

**Workspace:** `$HOME/.slop/pages/<repo-id>/<page-id>/`

```
page.json          task, tier assignment, base SHAs, close changes, status, timestamps
context.slop.md    read-only context for local agents; working slop for bundle-only agents
returned/          bundle-only reslopped blocks awaiting deslop
```

This lives under `$HOME/.slop/` rather than the cache: a page holds in-flight
work, and losing it costs something.

**Amending in flight** is the part that has to work. An agent that discovers it
needs `src/secrets.rs` two thirds of the way through a task must be able to
promote it into the page without rebuilding anything — the tower graph already
scored it, so promotion is a tier change plus an append, and `#SLOP_REQUEST`
is already the channel for asking.

**On close:** deslop `returned/` into the real files, honouring `SLOP_BASE_SHA`
drift detection exactly as today. Also compare the real page files against their
recorded base SHAs so local tool-using agents can edit those files directly;
record the direct and returned change sources in the closed manifest, then
refresh the project graph. Refuse an empty close unless `--allow-empty` is
explicit. That refresh is cheap only because of Stage 1 — a handful of touched
files re-parse and nothing else does.

---

## Stage 4 — teaching the agent

The machinery is useless if the agent does not know to reach for it.

- Extend `resources/how_to_slop*.md` with the paging protocol: build a project
  graph if none exists, identify the subset, open a page, work only in the
  page, close it.
- A `#SLOP_META "context-page"` block at the head of `context.slop.md` stating
  the task, the tier of every file, and what sits just outside the page — so
  the agent can see the edge of its own context and knows what it may request.
- Make `#SLOP_REQUEST` promote rather than re-bundle.

---

## Cross-cutting commitments

**Determinism.** Louvain visits nodes in index order and breaks ties by keeping
the current assignment; no randomization anywhere. An unchanged repo produces a
byte-identical graph, which is what makes the artifact diffable and the cache
trustworthy.

**Staleness.** The graph is only ever as fresh as its last refresh. Reads that
can tolerate drift use `load_project_graph`; anything that must be current
calls `refresh_project_graph`. Page close always refreshes.

**Secrets.** Pages are slops. They go through `secrets::enforce` on the same
terms, and `--allow-secrets`/`--redact` mean what they already mean.

**Budget.** Tier assignment proposes; `max_slop_bytes` disposes. When a page
will not fit, tiers degrade downward — tier 1 to outline before tier 0 is ever
touched — rather than dropping files off the end of a sorted list.

---

## Open questions

1. Should the tower graph cache be invalidated by project-graph changes, or
   carry its own hash of the contributing files? The second is more code and
   fewer surprises.
2. Does a page survive a `slop` process exit, or is it explicitly closed? The
   layout above assumes explicit close, which means orphaned pages need a
   `--page-prune`.
3. Co-change across renames: `--follow` is per-path and expensive. Worth it, or
   accept that a renamed file starts its history over?
