# Output contract and question catalog

This is **the product contract**: which questions chrono answers and their exact shape. Defined **before** the logic. Every answer is small, bounded JSON — never raw commits. JSON field names are English and never localized.

## Common envelope

```json
{
  "schema_version": 1,
  "question": "hotspots",
  "generated_at": "2026-09-19T12:00:00Z",
  "window": { "since": "2025-01-01", "until": null },
  "manifest": { "git_version": "2.50.1", "first_parent": "false", "bulk_threshold": "50" },
  "token_budget": { "max_items": 25, "returned": 25, "truncated": true },
  "result": { }
}
```

- **`token_budget`** is explicit: chrono trims to `max_items` and flags `truncated`.
- **`window`** always present: every metric is relative to a time window.
- **`manifest`** makes the answer reproducible and citable.
- **`result.hints`** (optional, first key of `result`, in the user's language): why a result is empty or partial when it is not an error — `top attr:<key>` or `stats <attr>` with a key no event has (or with no numeric values), `hotspots` without a git source, time `correlate` with a single source (suggests `--via`), `session_patterns` without Nod events, events with no time left out of `timeline` (also counted in `result.untimed`). A `--level` no event has, and a flag that does not apply to the command, are warned on stderr. Over MCP, the tools with a header (`top`, `stats`, `timeline`, `events`, `patterns`, `precede`, `informe`) show them, and an unknown `level`, as `⚠` lines (`meta.warnings` with `format=json`); the rest keep `hints` in the result. MCP `top attr:<key>` counts `(resto)` only over events that have the key, and those without it in a separate `(sin valor)` row.
- Lists that always come whole (`sources`, `phases`, `tickets`, `show`, `bench`) report `max_items = returned` and `truncated: false`.
- **Nothing is left out without a count.** Fields that say what a result does not cover:
  - `gaps`: `order` (`to_after_first_from` by default, `any` with `--any-order`), `units`, `covered`, `missing`, `from_without_key` (A events with no value of the key or of `--by`) and `units_without_time` (units whose A events have no time; neither covered nor missing).
  - `precede`: `episodes_total` and «before x/N» over every episode (`limit` only bounds the returned ones), `precursors_total` (distinct precursors seen; `common_precursors` brings the first 10), `anchors_without_time` (only when > 0) and, with `--join`, `anchors_without_key`. Each common precursor brings `active_episodes`, `expected` and `lift`, and `lift_base` says where the base rate comes from: `active_windows` (no `--join`: windows over the whole period with activity from the template's sources, from the rollups; `active_episodes` are the episodes whose previous window had that activity), `key_windows` (`--join K`: windows of each value of K that has an episode, with activity of that value; `active_episodes` are the episodes with some event of their value in the previous window; it also brings `lift_base_units`, `lift_base_events` and `lift_base_templated`, the population read) or `none` (rollups behind, or with `--join` the events of those values exceed the cap: 3M read / 200k templated; a hint says which). The three per-precursor fields are `null` when `lift_base` is `none` or the precursor has no base (not in the rollups).
  - `sources`: `stale_dims_events` (only when > 0: events kept with the dims or options from before, because the source could not be re-read whole) and `sync_error` (the last `sync` could not read that source; time and error).
- `events` lists in time order; `--order desc` (MCP `order=desc`) returns the newest first.

## Question catalog

| Command | Question | `result` |
| --- | --- | --- |
| `hotspots` | What's dangerous to touch? | list of `{path, changes, size_lines, score}` |
| `coupling <f>` | What breaks if I touch this? | `{for, coupled:[{b, support, confidence}]}` |
| `owners <path>` | Who owns this area? | `{owners:[{name,share}], bus_factor}` |
| `bugs` | Where do fixes concentrate + what kind? | `{categories:[…], hot_areas:[…], total_fixes}` |
| `churn` | What's moving now? | list of `{path, added, deleted}` in the window |
| `tickets <id>` | What code resolved ticket X? | `{ticket, commits:[…], files:[…], prs:[…]}` |
| `prs` | Which PRs exist and their state? | list of `{number, title, state, merged, is_bug, labels}` |
| `branches [base]` | What's the state of each branch? | `{base, current, branches:[{name, current, tip, age_days, ahead, behind, merged, stale, authors, bus_factor}]}` |
| `phases` | What were the project phases? | list of `{tag, date}` from tags/releases |
| `search <text>` | Which commits talk about X? | list of `{sha, subject, date}` (FTS5/BM25) |
| `similar <sha>` | Which commits are near-identical? | list of `{sha, subject, distance}` (SimHash) |
| `explain <file>[:A-B]` | Why is this code like this, and what changes with it? | `{file, from, to, file_lines, revision, summary:{commits, fixes_yes, fixes_maybe, not_indexed, oldest, newest}, commits:[{id, date, author, lines, fix, category?, title, tickets?, bulk?}], authors:[{author, lines}], changes_with:[{entity, commits, of, symbols?}]}`. `git blame` at the indexed revision (never the working tree) with user blame config neutralized; commits newest first, ties by sha; dates UTC; `fix`: `yes` = conventional `fix:`/revert, `maybe` = keyword only, `-` = not indexed; `changes_with` = entities in ≥2 of those (non-bulk) commits, a file's own symbols excluded, other files' symbols grouped under their file. Same index ⇒ same bytes. |
| `session_patterns` | What did each agent session/task cost, and did its commits hold? (Rust port; needs a `nod` source) | `{summary, basis, tasks:[…], sessions:[…], notes}` — see `docs/NOD.md` |

## `result` examples

**hotspots**
```json
{ "hotspots": [
  { "path": "internal/store/store.go", "changes": 142, "size_lines": 380, "score": 0.91 }
]}
```

**bugs** (relational + taxonomy)
```json
{ "total_fixes": 8111,
  "categories": [
    { "category": "network", "fixes": 1566, "top_files": ["src/…/socket.zig"], "examples": ["a1b2c3d","e4f5a6b"] }
  ],
  "hot_areas": [
    { "dir": "src/bun.js", "fixes": 900, "recent_fixes": 210, "recency_weight": 0.23, "examples": ["…"] }
  ]}
```

**coupling**
```json
{ "for": "auth/session.go", "coupled": [
  { "b": "auth/token.go", "support": 41, "confidence": 0.85 }
]}
```

**branches** (live from git, not the index)
```json
{ "base": "main", "current": "feature/x", "branches": [
  { "name": "feature/x", "current": true,
    "tip": { "sha": "a1b2c3d", "author": "Ada", "date": "2026-08-30 12:00:00 -0500", "subject": "add cache" },
    "age_days": 3, "ahead": 5, "behind": 12, "merged": false, "stale": false,
    "authors": [{ "name": "Ada", "commits": 5 }], "bus_factor": 1 }
]}
```

## Trimming rules

1. Order by relevance (score / support / fixes) and cut at `max_items`.
2. At most **2 examples** (SHAs) per group, never the diff.
3. Long text (subject) is truncated with `…`.
4. `truncated: true` whenever something was cut, so the AI knows there's more.
