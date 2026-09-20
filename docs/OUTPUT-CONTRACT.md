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
