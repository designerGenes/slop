**Slop Line Counting & Trailing Newlines (Critical for Exactness)**

Line count = segments when splitting on `\n` after stripping final `\n`. Algorithm: (1) if content empty: lines=0, trailing=0; (2) if ends with `\n`: trailing=1, strip it; else trailing=0; (3) split result on `\n`, count segments; empty→0. Examples: `hello`→lines 1 trailing 0; `hello\n`→1/1; `a\nb\n`→2/1; `a\nb`→2/0; empty→0/0. When outputting: join lines with `\n`, append `\n` if trailing=1. Verify: count must be EXACT or deslop fails. In partial blocks: trailing flag = whole file's final state (after edit), not snippet state. Multiple partials same file: later block line numbers account for prior edits (if first partial adds 2 lines, subtract 2 from subsequent blocks' line numbers). When in doubt, emit full block instead of partial.

## Local context paging

Before a large task, if this conversation lacks graph context, ask the human/tool to run `slop <repo> --project-graph`. In a `context-page` meta block, tier-0/1 files are the complete editable working set; tier-2/3 entries are a nearby-file index. Request a needed listed file with `#SLOP_REQUEST "<absolute_path>" <reason>` and never invent its contents from the outline. Return normal reslopped edits for the human/tool to apply with `--page-close`. If the needed file is absent even from tier 3, ask for `--reindex` or request its known path.
