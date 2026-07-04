# Soupify

A CLI tool that bundles source files into a single Markdown file for transmission to AI systems, then restores the AI's edits back to disk.

The core idea: instead of copy-pasting files into a chat window, `soupify` concatenates them with structured headers the AI can parse. The AI edits the content and returns a soup file; `soupify -d` applies those edits atomically.

---

## Installation

```bash
./install.sh
```

Installs the binary to `~/.local/bin/soupify` and creates `~/.config/soupify/config.yaml` with annotated defaults.

---

## Recipes

Ordered from simplest to most complex.

---

### 1. Bundle a few files

The baseline. Grab exactly the files you want to discuss or edit.

```bash
soupify src/auth.py src/models.py tests/test_auth.py
```

The soup is written to `~/.soupify/soupified/auth_models_test_auth.md`. Paste it into any AI chat. When the AI returns a soup, save it and run recipe 8 to apply the edits.

---

### 2. Bundle a whole directory (non-recursive)

Grab all immediate files in a folder — useful for a flat `src/` or a small module.

```bash
soupify src/
```

Without `-r`, only direct children of `src/` are included (no subdirectory traversal).

---

### 3. Bundle a directory tree

Recursively collect every source file under a path.

```bash
soupify -r src/
```

---

### 4. Exclude noise

Skip generated files, vendored code, or test fixtures.

```bash
soupify -r src/ -x '*.min.js' -x 'node_modules/' -x '__pycache__/'
```

Exclusion patterns support globs (`*.log`), folder names with trailing slash (`build/`), and regexes (`/test_\d+/`).

VCS/build directories (`.git`, `node_modules`, `target`, `dist`, `build`, `__pycache__`, `.venv`, etc.) are always pruned automatically and don't need `-x`. For everything else your project's `.gitignore` already tracks, use `--respect-gitignore` instead of hand-listing patterns:

```bash
soupify -r --respect-gitignore src/
```

This walks the directory the same way `git` would: any file or folder matched by a `.gitignore` found at or below the input path (including nested `.gitignore` files and `!negation` patterns) is skipped. Explicitly naming a gitignored file as a direct argument still soupifies it — only directory traversal is pruned, matching how tools like ripgrep behave. Set it permanently in config so you don't need the flag on every run:

```yaml
# ~/.config/soupify/config.yaml
respect_gitignore: true
```

---

### 5. Add a whole-repo code graph

Append a `#SOUP_META "repo-graph"` block that shows the entire repository's symbol graph, ranked by relevance to the files you're uploading. The AI can see how your files fit into the larger codebase — and which files it can request if it needs more context.

```bash
soupify -r -g src/
```

The graph covers the full git repo regardless of which files you selected. Files you uploaded become PageRank seeds so they rank highest in the map. Graph-containing soups get a `_graph` suffix in their filename.

Tune the graph's token budget:

```bash
soupify -r -g --graph-map-tokens 4096 src/
```

---

### 6. Add read-only context files

Include a file as full-text context the AI can read but must not edit or return in its soup. Useful for interface definitions, schemas, or config that informs the task but shouldn't be modified.

```bash
soupify src/main.py --context-file src/schema.py --context-file API.md
```

Context files appear in the soup with `#SOUP_READONLY true`. They do not affect the output filename.

---

### 7. Send the soup to a custom output location

```bash
soupify -r src/ -o ~/Desktop/my_project_soup/
# or
soupify -r src/ --soupify-to ~/Desktop/my_project_soup/
```

---

### 8. Apply an AI's returned soup (desoupify)

When an AI returns edited files as a soup, save it and run:

```bash
soupify -d returned.soup.md
```

Or let soupify find the right soup automatically by passing the files the AI edited:

```bash
soupify -d src/auth.py src/models.py
```

---

### 9. Preview an AI's edits before applying them

Always safe to run before a live desoupify. Shows a unified diff per file, makes no writes.

```bash
soupify -d --dry-run returned.soup.md
```

---

### 10. Selection by keyword — upload only relevant files

Build a local full-text index of the repo and select only the files that match your search terms. Useful when the repo is large and you know what you're looking for.

```bash
soupify -r --match "authentication" --match "session" src/
```

First run builds the index (a few seconds). Subsequent runs are incremental. Add `-g` to pair intelligent selection with a whole-repo graph:

```bash
soupify -r -g --match "authentication" --match "session" src/
```

Force a full reindex if files changed substantially:

```bash
soupify -r -g --match "authentication" --reindex src/
```

---

### 11. Selection by symbol — follow a function through the codebase

Select the file(s) that define a symbol plus all files that call it (callers) and all files it calls (callees). Deterministic — no index needed.

```bash
soupify -r --symbol "handle_login" src/
```

---

### 12. Selection by seed file with neighbor traversal

Anchor on a specific file and pull in all files it references or is referenced by, out to N hops in the tag graph.

```bash
# Seed file plus 1-hop neighbors (default)
soupify -r --seed src/auth.py src/

# Seed file plus 2-hop neighbors
soupify -r --seed src/auth.py --hops 2 src/
```

Combine with a graph and explain what was selected:

```bash
soupify -r -g --seed src/auth.py --hops 1 --explain-selection src/
```

`--explain-selection` adds a `#SOUP_META "selection"` block listing every selected file, its reason (Seed / Neighbor / Symbol / Match / Task), and its score.

---

### 13. Selection by natural-language task description (fuzzy)

Describe what you want to work on in plain English. The same BM25 index used by `--match` runs the prose query. Less deterministic than the other selectors — use `--match` or `--symbol` when reproducibility matters.

```bash
soupify -r -g --task "add rate limiting to the login endpoint" src/
```

Disable fuzzy mode entirely to enforce deterministic-only selection:

```yaml
# ~/.config/soupify/config.yaml
allow_fuzzy_task: false
```

---

### 14. Combine selectors

Selectors compose: seeds are always included (tier 0), then their graph neighbors (tier 1), then symbol resolution (tier 2), then keyword matches (tier 3), then the task query (tier 4). Each file appears only once, at its highest-priority tier.

```bash
# Anchor on two files, pull their neighbors, and also grab anything matching "rate_limit"
soupify -r -g \
  --seed src/auth.py \
  --seed src/middleware.py \
  --hops 1 \
  --match "rate_limit" \
  --explain-selection \
  src/
```

---

### 15. Cap the soup size

Enforce a hard ceiling on the serialized soup. Files beyond the budget are dropped (reported to stderr) rather than silently omitted.

```bash
soupify -r -g --match "parser" --max-soup-bytes 512000 src/
```

Set a permanent default:

```yaml
# ~/.config/soupify/config.yaml
max_soup_bytes: 524288  # 512 KiB
top_k: 8               # max files from selection
```

---

### 16. Scan for secrets before uploading

The secrets scanner runs automatically before serialization. Pattern-rule hits (private keys, AWS/Google/Twilio/Stripe/Slack/GitHub tokens, JWTs, bearer tokens) are **blocking**. High-entropy string hits are **warnings**.

Override an individual line that is a known false positive:

```python
INTERNAL_KEY = "abcdef..."  # soupify:allow-secret
```

Bypass the block gate for a one-off run:

```bash
soupify -r src/ --allow-secrets
```

Mask secret values in the soup without touching files on disk. Masked files are marked `#SOUP_READONLY true` and cannot be round-tripped via partial edits:

```bash
soupify -r src/ --redact
```

Set the default scan mode:

```yaml
# ~/.config/soupify/config.yaml
secret_scan: block    # warn (default) | block | off
redact_secrets: true
```

---

### 17. Confine where desoupify can write

By default, desoupify restricts writes to the common ancestor directory of the soup's file paths. Widen it explicitly if your project spans multiple roots:

```bash
soupify -d returned.soup.md --allow-root ~/projects/backend --allow-root ~/projects/frontend
```

---

### 18. Always-on graph via config

Turn on the graph for every soupify run without typing `-g` each time:

```yaml
# ~/.config/soupify/config.yaml
include_graph: true
graph_map_tokens: 3000
```

---

### 19. Full workflow — "ask the AI to fix a bug, apply the result"

```bash
# 1. Find the relevant files
soupify -r -g \
  --match "NullPointerException" \
  --match "UserService" \
  --explain-selection \
  src/

# 2. Inspect what was selected
#    (check ~/.soupify/soupified/ for the latest *_graph.md file)

# 3. Paste the soup into your AI, describe the bug, ask for a fix

# 4. Save the AI's returned soup as returned.md

# 5. Preview before applying
soupify -d --dry-run returned.md

# 6. Apply
soupify -d returned.md
```

---

## Desoupify reference

| Scenario | Command |
|---|---|
| Apply a soup file directly | `soupify -d path/to/file.soup.md` |
| Find the right soup by the files it contains | `soupify -d src/auth.py src/models.py` |
| Find the right soup by directory | `soupify -d src/` |
| Preview without writing | `soupify -d --dry-run path/to/file.soup.md` |
| Write to a non-default soup dir | `soupify -d -o ~/my-soups/ src/auth.py` |

---

## Config reference (`~/.config/soupify/config.yaml`)

| Key | Default | Description |
|---|---|---|
| `soupified_folder` | `~/.soupify/soupified` | Where soup files are written |
| `include_graph` | `false` | Always include the code graph |
| `respect_gitignore` | `false` | Always skip files/folders matched by the repo's `.gitignore` |
| `graph_map_tokens` | `2048` | Token budget for the graph block |
| `graph_token_model` | `o200k_base` | BPE model used to count graph tokens |
| `index_dir` | `~/.cache/soupify/index` | Location of the full-text selection index |
| `top_k` | `12` | Max files returned by selection |
| `max_soup_bytes` | `1048576` | Hard ceiling on serialized soup (1 MiB) |
| `selection_default_hops` | `1` | Default BFS radius for `--seed` |
| `allow_fuzzy_task` | `true` | Allow `--task` prose queries |
| `selection_provenance` | `false` | Always emit the selection meta block |
| `secret_scan` | `warn` | `warn` / `block` / `off` |
| `redact_secrets` | `false` | Mask secret values by default |
| `auto_desoupify` | `false` | Auto-apply soups landing in the watched folder |
| `warn_before_overwriting` | `false` | Prompt before overwriting on desoupify |
