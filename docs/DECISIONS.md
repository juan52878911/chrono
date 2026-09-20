# Decisions and deterministic manifest

## The manifest (`meta` in the index)

These decisions are **stored in the `.db`** so two machines produce the same index and so we know when to reindex:

| Key | v1 value | Why |
| --- | --- | --- |
| `schema_version` | 1 | migrate/reindex if it changes. |
| `git_version` | from env | rename detection (`-M`) is heuristic; pinning it makes results comparable. |
| `first_parent` | **false** | corrected empirically: `--first-parent` hid branch commits that do change files (undercount). |
| `no_merges` | true | count all real (non-merge) work, wherever it lives. |
| `mailmap_used` | true | author identities. |
| `bulk_threshold` | 50 | commits touching more files are excluded from coupling. |
| `last_sha` | — | watermark for incremental `sync`. |
| `repo_path` | abs path | so `sync` and per-repo config work without an argument. |

## Decision log (condensed ADR)

1. **One SQLite file, not Memgraph/Qdrant.** Personal scale; a portable `.db` beats distributed infra. **Firm.**
2. **Never embed diffs.** They embed poorly and cost; only NL text (messages/PRs), and only in v2. **Firm.**
3. **Stream `git log`, not libgit2.** git already handles the edge cases; constant RAM. **Firm.**
4. **Permanently out of scope:** mass `git blame`, symbol-level history, predicted "regression risk". That's where these projects die. **Firm.**
5. **"Most common bugs" = relational (where) + keyword taxonomy (what kind).** Token-frequency clustering was too weak; a configurable `bug_categories` map yields semantic categories (memory-safety, crash, network…). *(Recalibrated after a real opencode test.)*
6. **Classifier: rules (Level 0) in v1.** A trained local classifier and Jev need a labeled dataset + eval metric first; v2.
7. **`serve` is a second product** → scaling phase, not v1.
8. **Ports only where 2+ adapters will exist:** Source, Tracker, Classifier. Store/Embedder go direct for now.
9. **Go for v1** (delivery speed; Rust's memory argument was about the deferred service).
10. **Sized for ~500k commits.** File sizes computed only for the top ~4000 most-changed files (fast on huge repos; Bun = 17,678 commits).
11. **Positioning by capabilities, not "99% fewer tokens".** Honest saving vs `git log --grep`+LLM is ~3–5×; the big win is determinism + not saturating context.
12. **English by default** (OSS standard), Spanish via locale/`--lang`/`CHRONO_LANG`. JSON output never localized.
13. **Forge prefers `upstream`** on forks (PRs live upstream); drops heavy fields to avoid GitHub's GraphQL node limit; surfaces errors (no silent zero).

## Deterministic classification (bugs/tickets)

Prose intent can't be made deterministic, but it can be made **reproducible and explicit**:
1. **Rules in `.chrono/config.json`** (versionable): `fix_keywords`, `ticket_patterns`, `exclude_globs`, `bug_labels`, `bug_categories`.
2. **Git-native signals** (100% deterministic): reverts via the `This reverts commit <sha>` body; conventional-commit prefixes.
3. **Forge labels** are the real ground truth for "is this a bug" — fires where the team labels/links PRs; requires the Tracker port (implemented).

## Gaps to close before/while building

- Author identity merging beyond mailmap (one human, several emails).
- Schema migration + reindex on embedding-model change (v2).
- Classifier evaluation (labeled dataset + metric + threshold) before moving past Level 0.
- Write concurrency documented (WAL, single writer).
