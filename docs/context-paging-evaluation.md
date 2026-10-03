# Context paging: evaluation and decision

**Status: not pursued. Evaluated over four benchmark rounds and three offline
analyses; the mechanism cannot pay for its own delivery cost in an agentic
loop, and no mechanism that filters or compresses content *within a single
session* reaches a material share of session cost either (§11).** This document records
what was tested, what was found, and what would have to be different for the
conclusion to change, so that the work is not repeated from scratch.

The premise of slop's single-shot bundling is untouched by any of this. See
"What this does not say".

## 1. The idea under test

Give a coding agent a ranked subset of a repository plus a graph of how those
files connect to the rest, instead of letting it discover the relevant files
itself. Delivered two ways: a source bundle (`--page-open`) and a manifest
(`--page-open --manifest`: paths, tiers, symbols and entry anchors, no source).

The expected saving was that the agent would stop paying for context it does
not need.

## 2. The hidden assumption, and why it does not hold

The premise assumes the baseline is *the whole repository in context*. In an
agentic loop it never is. The agent starts near empty and pulls files in with
grep and read, so the real comparison is a lazily-found subset against an
eagerly-delivered one — and in manifest mode the agent reads the same files
either way, because the page hands it paths, not contents.

Context paging is therefore not competing with bulk context. It competes with
grep. What remains to save is only search traffic and **wrong reads** — whole
files opened, found irrelevant, and resident for every later turn.

## 3. The decisive arithmetic

Across 24 mechanically sampled tasks on three repositories, measuring every
tool result entering context as tokens × turns resident, at the cache-read rate:

| repo | wrong-read residency per task | manifest residency | ratio |
| --- | ---: | ---: | ---: |
| ruff | $0.036 | $0.104 | 2.9× underwater |
| gh | $0.016 | $0.082 | 5.1× underwater |
| hugo | $0.019 | $0.071 | 3.7× underwater |

(Wrong-read figures are means per task from the bucket totals; manifest figures
are per-repo medians. The mixed statistic does not change the sign or the order
of magnitude.)

The page costs three to five times more than the entire recoverable bucket —
that is, more than it could save if it prevented **every** wrong read on every
task. This is not a capture-rate or tuning problem. Shrinking the page to clear
ruff's number would mean roughly a third of the current page (≈11 files, from
the 2.9× ratio), and coverage at thirty-two files is already only 0.30 there.

## 4. Why each delivery mechanism failed

**The graph's unique surface is empty.** Classifying all 72 target files by what
could have found them from the task statement alone:

| repo | lexical (grep finds it) | structural (one `ls` away) | non-lexical (graph only) |
| --- | ---: | ---: | ---: |
| ruff | 18 (47%) | 19 (50%) | 1 (3%) |
| gh | 27 (93%) | 2 (7%) | 0 |
| hugo | 20 (77%) | 6 (23%) | 0 |

The value proposition was the edges grep cannot traverse — registry membership,
dispatch, generated/source pairs, co-change. On this sample they account for
3% of targets on one repository and none on the other two. Ruff's distinctive
class is structural, which `ls` already reaches.

**Naming the right files does not prevent the wrong reads.** On gh the manifest
had target recall of 1.00 — it named every file the agent ended up editing —
and its steerable share of wrong reads was 0.000. The wrong reads were not
navigational. The agent was not lost; it was reading neighbouring code to learn
conventions. A list of correct filenames is not a substitute for that, which
places a ceiling on *any* file-selection approach, not only this one.

**A minimal note captures nothing.** A ≤5-path co-change/registry note costs
40–80× less than the page ($0.001–$0.002 residency) and has a median target
recall of 0.00 in every repository — the direct consequence of there being no
non-lexical edges for it to report.

**The effect is not predictable.** Leave-one-out on n=24, using only features
available before a run: best 0.71 at predicting above/below-median navigation
calls, 0.58 at orientation share; best two-feature combination 0.67. The
repo-level structural signal ordered three repositories correctly but n=3 is a
hypothesis, not a result.

## 5. What the benchmark rounds measured, and how the number moved

| round | published headline | after correction |
| --- | --- | --- |
| PYI061 (curated) | orientation cache-read −44.6% | −13.2% in dollars, oracle-seeded |
| B043 (curated) | orientation cache-read −75.7% | −33.5% in dollars, oracle-seeded |
| gh-unarchive (curated) | orientation cache-read −11.3% | −3.3% in dollars; held-out recall 0.00 manifest / 0.20 plain (one run of five passed) |
| 24 sampled tasks | — | recoverable ceiling below delivery cost on all three repos |

The estimate fell at every step where a bias was removed. That is the signature
of converging on a small true effect from an inflated start.

## 6. Methodological errors, recorded so they are not repeated

Five distinct defects. The first four are closed by mechanical checks rather
than by resolving to be careful; the fifth is a reasoning error, closed by a
rule:

1. **Prompt asymmetry.** The PYI061 manifest prompt was missing a `## Notes`
   block the plain prompt had. Closed by hashing the shared preamble *and* the
   anti-cheating-through-notes tail, plus `bench/preflight_path_symmetry.py`,
   which diffs every repo path each prompt names across the whole file. It
   currently blocks the curated specs on rerun: both name a seed file in the
   manifest arm only, which is exactly the bias it was built to catch.
2. **Oracle contract mismatch.** The gh held-out test drove a deprecated
   `--confirm` alias that neither prompt mentioned; nine of ten runs failed for
   it. Closed by `bench/preflight_contract.py`: flags are always required in
   both prompts, and exported identifiers the historical test references are
   required unless they already exist at the base commit. On the gh spec it
   reports exactly one missing token, `--confirm`.
3. **Task selection on the dependent variable.** Every curated task was chosen
   because its repository has a rigid registry convention — selection on the
   property the system exploits. Quantified: hand-picked ruff tasks showed 4/4
   target coverage; mechanically sampled ruff tasks, same repo and same rule,
   showed 0.09. Closed by frozen mechanical sampling.
4. **Seed provenance.** The ruff plain prompts never named a sibling seed while
   the manifest prompts named the exact file, so the manifest arm received a
   hint drawn from the answer. Closed by a path-symmetry check across the whole
   prompt, workflow section included.
5. **Predicting where cost lives from turn counts instead of measuring
   content.** After paging closed, the next hypothesis was that the build-test-fix
   loop held the money, because it is 76–92% of a session's turns. Measured,
   build and test *output* is 3.5% of session residency (11% in the worst repair
   run). Repair work drives session length; the cost is the context re-read on
   every one of those turns, not the compiler's words. Closed by decomposing
   content before proposing a mechanism.

Separately, the reported metric was orientation cache-read *tokens*, which
overstated the dollar effect by roughly 3×. Report dollars.

At least three alarming intermediate results turned out to be instrument rather
than finding: a structural-hub promotion that changed tier labels without
changing page contents, the gh held-out failure above, and a 0/24 seed-agreement
figure that bundled two real slop bugs with a metric that measured the wrong
thing (see §7).

## 7. Bugs found, and fixed, along the way

- `query_index` passed task strings to Tantivy's query parser raw. Prose
  containing hyphens or parentheses failed to parse, so 18 of 24 tasks produced
  no seed at all. Fixed with a query sanitizer and tests. This would have
  affected any real user typing a natural-language task.
- The full-text index went stale across checkouts in a reused worktree, so
  selection returned files that no longer existed.
- `analyze.py` recognised only `cargo`/`INSTA_UPDATE` as build commands and
  classified every `go build`/`go test` as nothing, and missed `timeout`
  wrappers. Now polyglot.

Seed *file identity* was the wrong metric. Corrected, slop's own task-derived
seed yields comparable or slightly better page coverage than an oracle seed on
all three repositories (ruff 0.30 vs 0.09), though a worse best-target rank on
ruff and hugo.

## 8. What this does not say

- It does not touch **single-shot bundling**, slop's founding use case. Where
  you must choose contents up front and there is no retrieval loop, the premise
  — subset plus graph instead of the whole repository — is correct and the
  saving is large. Context paging carried that premise across a boundary into a
  loop where the agent had already solved retrieval.
- It does not generalise to agents **without good search**, or to situations
  where the **context window is the binding constraint** rather than cost.
- Scope: three repositories, one model (`claude-haiku-4.5`), 24 sampled plus 3
  curated tasks, one task shape among the curated set.
- Correctness was never affected either way. Held-out recall was 1.00/1.00 or
  0.00/0.00 in every paired round; paging never improved it and never harmed
  it. The entire case was cost.

## 9. What would have to be true to revisit

- A codebase where non-lexical edges are common rather than ~0% — plausible
  candidates are heavy code generation, dependency-injection containers, or
  config-driven dispatch, none of which were in this sample.
- Delivery cost driven close to zero, which the note attempted and which failed
  for a different reason (nothing to say), not because it was too expensive.
- An agent whose wrong reads are navigational rather than exploratory. The gh
  result — recall 1.00, steerable share 0.000 — is the measurement to re-run
  first, because it is the one that generalises beyond this design.

## 10. What was kept

The evaluation apparatus: held-out oracles scoring against historical commits,
frozen mechanical task sampling, pre-registered stopping criteria, paired runs
with comparability gating, mechanical prompt-symmetry preflight
(`preflight_path_symmetry.py`), mechanical oracle-contract preflight
(`preflight_contract.py`), and a polyglot cost classifier. None of it is
specific to context paging.

Plus two fixes surfaced by the work: the query sanitizer (§7), and the
build-verb classifier in the harness.

## 11. The wider question: can session cost be reduced from here at all?

Asked after paging closed, and answered over 54 runs (24 sampled plain-arm, 30
curated paired) by decomposing every tool result entering context as tokens ×
turns resident.

**The residency cost model is correct.** Cache-read grows linearly with
accumulated context in every run — median R² 0.912 sampled, 0.966 curated, and
**no resets or plateaus in any of the 54 streams**. No provider-side compaction
or cache-TTL expiry is visible. The bucket model accounts for 97.6% of sampled
session dollars.

Session decomposition, sampled median session $0.1995:

| bucket | share of session |
| --- | ---: |
| fixed floor (system prompt + tool definitions) | 36.2% |
| necessary file reads | 15.6% |
| the agent's own messages | 13.5% |
| wrong reads | 7.0% |
| search traffic | 3.9% |
| build and test output | 3.5% |
| edit echoes | ~0% |

**The largest bucket is not addressable from here.** Over a third of every
session is the system prompt and tool definitions, resident from the first turn
to the last; with the agent's own reasoning that is roughly half the bill, and
both belong to whoever configures the agent harness. Reducing the tool surface
is the highest-leverage cost lever available on an agent session, and it is a
configuration decision rather than a feature.

Five candidate mechanisms were priced against a pre-registered bar of 10% of
median session cost ($0.0199). All are inline filters with ~zero resident
delivery cost, so the bar is the only binding constraint:

| lever | ceiling | vs bar |
| --- | ---: | ---: |
| turn reduction (adjacent reads, build→test) | $0.0175 | 0.88× |
| read deduplication | $0.0044 | 0.22× |
| symbol-level reads (±40 lines of edits) | $0.0027 | 0.14× |
| build-output noise filter | $0.0002 | 0.01× |
| repeated-diagnostic suppression | $0.0000 | 0.00× |
| eviction (best single point) | $0.0000 | 0.00× |

**None clears the bar.** Turn reduction is the only one within an order of
magnitude, and its ceiling is undercounted by construction — it counts only
adjacent read→read and build→test pairs — but raising it further requires
changing agent behaviour rather than adding a tool. Eviction's break-even
condition (`T > 12.5·(C−X)/X`) is satisfied in 7 of 24 sampled runs and 5 of 30
curated.

One figure worth re-checking if this is ever reopened: symbol-level reading
recovers only 7% of the necessary-read bucket, implying 93% of read tokens fall
within ±40 lines of an eventual edit. That is either evidence that agents
already read narrowly, or an artefact of small sampled files. Neither answer
changes the verdict, since the whole bucket is 15.6% and none of it is fully
recoverable.

**Conclusion, scoped precisely: agent session cost is not materially
addressable by filtering or compressing content within a single session.** All
five levers above work that way, and none clears the bar. The single-shot
bundling case — choosing contents up front where there is no retrieval loop —
remains untouched, and remains where the premise is correct.

**What this section does *not* cover.** Session cost is `Σ(context resident at
each turn)`, and with context growing linearly a session of N turns costs about
`g·N²/2`. Every lever priced here shaves `g`. **Session partitioning — doing the
same work across k shorter sessions — divides the quadratic term instead**,
giving `g·N²/(2k) + h·N` for a handoff cost h. On B043 p1-plain (115 steps,
$0.6449) a split at the first edit predicts `(900 + 7225)/13225 = 0.61`, a 39%
saving against a best-lever figure of 8.8%. That term was never measured in this
round and the conclusion above should not be read as covering it; it is under
test separately.

Note also the structural reason paging failed, which partitioning does not
share: from outside the agent loop slop could only **add** files to context,
never remove one. A manifest arrives on top of whatever the agent decides to
read. Partitioning is the first mechanism in this project that can bound what
enters context rather than recommend it.
