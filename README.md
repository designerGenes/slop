```
     ▗▟████▙▖    ▗▟███▖  ▐█▌     ▗▟███▙▖  ▗▟███▙▖
   ▗▟██▀██▀██▙▖  ▐▛▀▀▀▘  ▐█▌     ▐█▌ ▐█▌  ▐█▌ ▐█▌
   ▐████▄▄████▌  ▝▀▀██▖  ▐█▌     ▐█▌ ▐█▌  ▐████▛▘
   ▝▜████████▛▘  ▝███▛▘  ▐████▖  ▝▜███▛▘  ▐█▌
```

# slop

A CLI tool that bundles source files into a single Markdown file for transmission to AI systems, then restores the AI's edits back to disk.

The core idea: instead of copy-pasting files into a chat window, `slop` concatenates them with structured headers the AI can parse. The AI edits the content and returns a slop file; `slop -d` applies those edits atomically.

---

## Installation

```bash
./install.sh
```

Installs the binary to `~/.local/bin/slop` and creates `~/.config/slop/config.yaml` with annotated defaults.

---

## Recipes

Ordered from simplest to most complex.

---

### 1. Bundle a few files

The baseline. Grab exactly the files you want to discuss or edit.

```bash
slop src/auth.py src/models.py tests/test_auth.py
```

The slop is written to `~/.slop/slopified/auth_models_test_auth.md`. Paste it into any AI chat. When the AI returns a slop, save it and run recipe 8 to apply the edits.

---

### 2. Bundle a whole directory (non-recursive)

Grab all immediate files in a folder — useful for a flat `src/` or a small module.

```bash
slop src/
```

Without `-r`, only direct children of `src/` are included (no subdirectory traversal).

---

### 3. Bundle a directory tree

Recursively collect every source file under a path.

```bash
slop -r src/
```

---

### 4. Exclude noise

Skip generated files, vendored code, or test fixtures.

```bash
slop -r src/ -x '*.min.js' -x 'node_modules/' -x '__pycache__/'
```

Exclusion patterns support globs (`*.log`), folder names with trailing slash (`build/`), and regexes (`/test_\d+/`).

VCS/build directories (`.git`, `node_modules`, `target`, `dist`, `build`, `__pycache__`, `.venv`, etc.) are always pruned automatically and don't need `-x`. For everything else your project's `.gitignore` already tracks, use `--respect-gitignore` instead of hand-listing patterns:

```bash
slop -r --respect-gitignore src/
```

This walks the directory the same way `git` would: any file or folder matched by a `.gitignore` found at or below the input path (including nested `.gitignore` files and `!negation` patterns) is skipped. Explicitly naming a gitignored file as a direct argument still slopifies it — only directory traversal is pruned, matching how tools like ripgrep behave. Set it permanently in config so you don't need the flag on every run:

```yaml
# ~/.config/slop/config.yaml
respect_gitignore: true
```

Use a project `.slopignore` for slop-specific exclusions. It accepts `.gitignore`-style patterns, plus `+ pattern` or `slopinclude pattern` to force a matching file into every slop. Includes override `.slopignore` and `.gitignore` filtering, bypass shallow traversal, and may name an external file through `$HOME`:

```text
generated/
+ generated/required-schema.json
slopinclude $HOME/shared/api-contract.md
```

Files explicitly listed on the command line and matched by one or more include rules are bundled only once.

The complete selection precedence is defined in [`resources/slop-rules.yaml`](resources/slop-rules.yaml), a commentable, versioned DSL embedded in the binary. It distinguishes direct file requests, recursive directory walks, `slop .`, and named shallow directories. `.slopignore` supplies the familiar gitignore-style patterns within the directory-walk rules; a missing `.slopignore` means `slop .` performs its normal shallow walk, not a recursive or include-directed walk. `+ pattern` and `slopinclude pattern` run after ordinary traversal and take precedence over `.slopignore` and `.gitignore` matches.

When you provide only explicit file paths, you can make that list authoritative and skip the repository's `.slopignore` file, including its `slopinclude` directives:

```bash
slop --ignore-slopignore src/auth.py src/models.py
```

Enable this automatically for all-file statements in `~/.config/slop/config.yaml`:

```yaml
skip_slopignore_for_full_statement: true
```

The setting applies only when every positional input is an explicit file. Use `--ignore-slopignore` to bypass `.slopignore` for any invocation, including directory walks.

Because includes supersede ignore rules, a leading `*` turns `.slopignore` into an allowlist — ignore everything, then name the exceptions:

```text
*

+ someFile
+ someDirectory/
+ someOtherDirectory/*.md
```

---

### 5. Add a whole-repo code graph

Append a `#SLOP_META "repo-graph"` block that shows the entire repository's symbol graph, ranked by relevance to the files you're uploading. The AI can see how your files fit into the larger codebase — and which files it can request if it needs more context.

```bash
slop -r -g src/
```

The graph covers the full git repo regardless of which files you selected. Files you uploaded become PageRank seeds so they rank highest in the map. Graph-containing slops get a `_graph` suffix in their filename.

Tune the graph's token budget:

```bash
slop -r -g --graph-map-tokens 4096 src/
```

---

### 6. Add read-only context files

Include a file as full-text context the AI can read but must not edit or return in its slop. Useful for interface definitions, schemas, or config that informs the task but shouldn't be modified.

```bash
slop src/main.py --context-file src/schema.py --context-file API.md
```

Context files appear in the slop with `#SLOP_READONLY true`. They do not affect the output filename.

---

### 7. Send the slop to a custom output location

```bash
slop -r src/ -o ~/Desktop/my_project_slop/
# or
slop -r src/ --slop-to ~/Desktop/my_project_slop/
```

---

### 8. Apply an AI's returned slop (deslop)

When an AI returns edited files as a slop, save it and run:

```bash
slop -d returned.slop.md
```

Or let slop find the right slop automatically by passing the files the AI edited:

```bash
slop -d src/auth.py src/models.py
```

You can also paste a complete slop document directly into the terminal. Run `slop -d` with no selector to open **Manual deslop mode** in the current terminal window. It opens as a blank editor with a status bar showing `Ctrl-D to deslop` and `Ctrl-C to cancel`; arrow keys, Home/End, Backspace, Delete, and pasted text all work normally.

```text
slop -d
#SLOP "/absolute/path/to/file.swift" #SLOPED_LINES 1 #SLOP_TRAILING_NEWLINE 1
updated content
Ctrl-D
```

Piped input works the same way:

```bash
cat returned.slop.md | slop -d
```

---

### 9. Preview an AI's edits before applying them

Always safe to run before a live deslop. Shows a unified diff per file, makes no writes.

```bash
slop -d --dry-run returned.slop.md
```

---

### 10. Selection by keyword — upload only relevant files

Build a local full-text index of the repo and select only the files that match your search terms. Useful when the repo is large and you know what you're looking for.

```bash
slop -r --match "authentication" --match "session" src/
```

First run builds the index (a few seconds). Subsequent runs are incremental. Add `-g` to pair intelligent selection with a whole-repo graph:

```bash
slop -r -g --match "authentication" --match "session" src/
```

Force a full reindex if files changed substantially:

```bash
slop -r -g --match "authentication" --reindex src/
```

---

### 11. Selection by symbol — follow a function through the codebase

Select the file(s) that define a symbol plus all files that call it (callers) and all files it calls (callees). Deterministic — no index needed.

```bash
slop -r --symbol "handle_login" src/
```

---

### 12. Selection by seed file with neighbor traversal

Anchor on a specific file and pull in all files it references or is referenced by, out to N hops in the tag graph.

```bash
# Seed file plus 1-hop neighbors (default)
slop -r --seed src/auth.py src/

# Seed file plus 2-hop neighbors
slop -r --seed src/auth.py --hops 2 src/
```

Combine with a graph and explain what was selected:

```bash
slop -r -g --seed src/auth.py --hops 1 --explain-selection src/
```

`--explain-selection` adds a `#SLOP_META "selection"` block listing every selected file, its reason (Seed / Neighbor / Symbol / Match / Task), and its score.

---

### 13. Selection by natural-language task description (fuzzy)

Describe what you want to work on in plain English. The same BM25 index used by `--match` runs the prose query. Less deterministic than the other selectors — use `--match` or `--symbol` when reproducibility matters.

```bash
slop -r -g --task "add rate limiting to the login endpoint" src/
```

Disable fuzzy mode entirely to enforce deterministic-only selection:

```yaml
# ~/.config/slop/config.yaml
allow_fuzzy_task: false
```

---

### 14. Combine selectors

Selectors compose: seeds are always included (tier 0), then their graph neighbors (tier 1), then symbol resolution (tier 2), then keyword matches (tier 3), then the task query (tier 4). Each file appears only once, at its highest-priority tier.

```bash
# Anchor on two files, pull their neighbors, and also grab anything matching "rate_limit"
slop -r -g \
  --seed src/auth.py \
  --seed src/middleware.py \
  --hops 1 \
  --match "rate_limit" \
  --explain-selection \
  src/
```

---

### 15. Cap the slop size

Enforce a hard ceiling on the serialized slop. Files beyond the budget are dropped (reported to stderr) rather than silently omitted.

```bash
slop -r -g --match "parser" --max-slop-bytes 512000 src/
```

Set a permanent default:

```yaml
# ~/.config/slop/config.yaml
max_slop_bytes: 524288  # 512 KiB
top_k: 8               # max files from selection
```

---

### 16. Scan for secrets before uploading

The secrets scanner runs automatically before serialization. Pattern-rule hits (private keys, AWS/Google/Twilio/Stripe/Slack/GitHub tokens, JWTs, bearer tokens) are **blocking**. High-entropy string hits are **warnings**.

Override an individual line that is a known false positive:

```python
INTERNAL_KEY = "abcdef..."  # slop:allow-secret
```

Bypass the block gate for a one-off run:

```bash
slop -r src/ --allow-secrets
```

Mask secret values in the slop without touching files on disk. Masked files are marked `#SLOP_READONLY true` and cannot be round-tripped via partial edits:

```bash
slop -r src/ --redact
```

Set the default scan mode:

```yaml
# ~/.config/slop/config.yaml
secret_scan: block    # warn (default) | block | off
redact_secrets: true
```

---

### 17. Confine where deslop can write

By default, deslop restricts writes to the common ancestor directory of the slop's file paths. Widen it explicitly if your project spans multiple roots:

```bash
slop -d returned.slop.md --allow-root ~/projects/backend --allow-root ~/projects/frontend
```

---

### 18. Always-on graph via config

Turn on the graph for every slop run without typing `-g` each time:

```yaml
# ~/.config/slop/config.yaml
include_graph: true
graph_map_tokens: 3000
```

---

### 19. Full workflow — "ask the AI to fix a bug, apply the result"

```bash
# 1. Find the relevant files
slop -r -g \
  --match "NullPointerException" \
  --match "UserService" \
  --explain-selection \
  src/

# 2. Inspect what was selected
#    (check ~/.slop/slopified/ for the latest *_graph.md file)

# 3. Paste the slop into your AI, describe the bug, ask for a fix

# 4. Save the AI's returned slop as returned.md

# 5. Preview before applying
slop -d --dry-run returned.md

# 6. Apply
slop -d returned.md
```

---

## Deslop reference

| Scenario | Command |
|---|---|
| Apply a slop file directly | `slop -d path/to/file.slop.md` |
| Find the right slop by the files it contains | `slop -d src/auth.py src/models.py` |
| Find the right slop by directory | `slop -d src/` |
| Preview without writing | `slop -d --dry-run path/to/file.slop.md` |
| Write to a non-default slop dir | `slop -d -o ~/my-slops/ src/auth.py` |

---

## Config reference (`~/.config/slop/config.yaml`)

| Key | Default | Description |
|---|---|---|
| `slopified_folder` | `~/.slop/slopified` | Where slop files are written |
| `include_graph` | `false` | Always include the code graph |
| `respect_gitignore` | `false` | Always skip files/folders matched by the repo's `.gitignore` |
| `graph_map_tokens` | `2048` | Token budget for the graph block |
| `graph_token_model` | `o200k_base` | BPE model used to count graph tokens |
| `index_dir` | `~/.cache/slop/index` | Location of the full-text selection index |
| `top_k` | `12` | Max files returned by selection |
| `max_slop_bytes` | `1048576` | Hard ceiling on serialized slop (1 MiB) |
| `selection_default_hops` | `1` | Default BFS radius for `--seed` |
| `allow_fuzzy_task` | `true` | Allow `--task` prose queries |
| `selection_provenance` | `false` | Always emit the selection meta block |
| `secret_scan` | `warn` | `warn` / `block` / `off` |
| `redact_secrets` | `false` | Mask secret values by default |
| `auto_deslop` | `false` | Auto-apply slops landing in the watched folder |
| `warn_before_overwriting` | `false` | Prompt before overwriting on deslop |
