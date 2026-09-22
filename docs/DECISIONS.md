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
4. **Symbol history: cheap yes, AST no** *(revised 2026-09-20)*. Symbol-level metrics are IN scope **only via the hunk's function name** (git's `@@ … @@ <funcname>`, with chrono's own `xfuncname` regex so results don't depend on the user's git version). A symbol is another `Touch` with `entity_type="symbol"` (`file#func`); opt-in (`init --symbols`), off by default (the cost is `init` time from `-p`, not binary size). **Still permanently out of scope:** parsing the AST of every blob in history, mass `git blame`, call graphs / function renames / complexity, predicted "regression risk". tree-sitter, if ever, is an opt-in Cargo feature for a few languages, only if measured misattribution warrants it. See `DESIGN-GENERAL-CORE.md §7 (S0–S3)`.
5. **"Most common bugs" = relational (where) + keyword taxonomy (what kind).** Token-frequency clustering was too weak; a configurable `bug_categories` map yields semantic categories (memory-safety, crash, network…). *(Recalibrated after a real opencode test.)*
6. **Classifier: rules (Level 0) in v1.** A trained local classifier and Jev need a labeled dataset + eval metric first; v2.
7. **`serve` is a second product** → scaling phase, not v1.
8. **Ports only where 2+ adapters will exist:** Source, Tracker, Classifier. Store/Embedder go direct for now.
9. **Go for v1** (delivery speed; Rust's memory argument was about the deferred service).
10. **Sized for ~500k commits.** File sizes computed only for the top ~4000 most-changed files (fast on huge repos; Bun = 17,678 commits).
11. **Positioning by capabilities, not "99% fewer tokens".** Honest saving vs `git log --grep`+LLM is ~3–5×; the big win is determinism + not saturating context.
12. **English by default** (OSS standard), Spanish via locale/`--lang`/`CHRONO_LANG`. JSON output never localized.
13. **Forge prefers `upstream`** on forks (PRs live upstream); drops heavy fields to avoid GitHub's GraphQL node limit; surfaces errors (no silent zero).
14. **Rust for the general core** *(2026-09-20, supersedes #9 for v2)*. The Go v0.1.1 binary stays the stable one until the Rust port reaches parity; the port lives in `rust/`. Rust buys parallel ingest with controlled memory for million-line traces and bit-stable integer JEV inference. Contract renamed to `entity`/`id` (`schema_version: 2`); v1→v2 migration is **reindex**, not in-place. See `DESIGN-GENERAL-CORE.md`.
15. **Log adapters (R3): jsonl, csv, textlog — zero external deps.** Each is an isolated crate implementing `Source`, registered in the CLI registry. `detect` is deliberately conservative so adapters never steal each other's files: csv is extension-gated to `.csv`/`.tsv` (never guesses CSV from content — a JSON line has commas too); textlog rejects any first line that matches no preset. textlog parses syslog (RFC5424/RFC3164) and nginx combined **by hand — no `regex` crate**, to keep the ~1.6 MB binary (Go was 6.7 MB). RFC3164 carries no year → taken from config `year`, else a fixed 1970 (deterministic, never the wall clock). A fourth adapter, **changelog** (Keep a Changelog), maps each version section to a `kind="release"` event and, unlike the append-only file adapters, uses a **content-hash watermark** (a changelog is edited at the top, so a byte offset is meaningless): same hash → nothing new, different hash → `Diverged` and the CLI re-ingests that source. Utilities (`simhash`, `timeutil`) are duplicated per adapter crate on purpose (crates stay independent), matching the jsonl precedent.
16. **Multi-source in one `.chrono/`** *(the real payoff of the general core)*. The `sources` table is populated per source, each with its own watermark; `chrono add <path>` ingests an extra source without wiping the index; `sync` iterates every source and reindexes only the one that diverged. On `sync`, git's forge/size refresh runs **only for git sources that actually re-ingested** (a log source getting new events no longer re-hits the GitHub API for an unchanged git source). `source_id = <kind>:<abs path>` — the same value events carry, so `correlate` and per-source queries line up. **Git parity is preserved** (verified on rustworkx + bun: hotspots top unchanged, init+sync idempotent, ~8s init at 17.7k commits). File-source divergence is caught two ways: the file gets **shorter** than the stored offset (truncation / rotate-to-smaller), **or** the hash of its consumed prefix changes. The prefix is the first `min(4096, off)` bytes (stored in the watermark as `off:<n>|p:<hex>`), hashed up to the offset — not up to the file length — so a plain append never falsely diverges, even on files under 4 KB. Honest limit of this heuristic: it only sees rewrites **within the first 4 KB**; an in-place edit past byte 4096 of an already-long file is not detected (the `4096` cap trades completeness for a cheap fixed-cost check). Old watermarks without `|p:` skip the hash check (backward-compatible). Second known limit: 2+ git repos in one index fight over the file-`deleted` flag (entities are global); the supported shape is 1 git + N log sources, whose entities aren't `type='file'`.

Determinism, precisely: two ingests of the same source produce **identical content** (events, touches, entities, labels, blob_lines — verified by dumping and hashing). The `.db` file is byte-for-byte identical **only when `now` is pinned** via the `CHRONO_NOW` env var (epoch seconds), because `sources.last_sync_at` records wall-clock time; without it the two files differ only in that one timestamp. The design's "hash the `.db` after VACUUM" reproducibility test must set `CHRONO_NOW`. (c) the git `bugs`/`hotspots` classifier still runs over log events, so those queries are only meaningful on a git-bearing index; `level` vocab isn't normalized across adapters (csv keeps the raw value upper-cased; syslog/nginx use the RFC severity names).

Per-source adapter options live in `.chrono/config.json` under `source_options` (keyed by the source's absolute path or its basename), e.g. `{"source_options": {"app.syslog": {"preset": "syslog", "year": "2024"}, "incidents.csv": {"time_column": "when", "delimiter": ";"}}}`. The CLI resolves them per source and passes them to the adapter's `open` (`Config::source_options_for`); with no entry, the adapter autodetects. This is what makes CSV column overrides and textlog `preset`/`year` reachable without code.
17. **Log-native queries read raw events; rollups are a separate wave.** `timeline` (counts per time bucket, optional `--by level|kind`), `top <dim>` (`level`|`kind`|`actor`|`entity`|`attr:<key>`), `correlate <id>` (events from *other* sources within ±Δt) query `events`/`touches` directly — correct and fast at current scale, with fully-ordered deterministic output. All three are **bounded** (`--limit`, default 500 for `timeline`/`top`, 25 for `correlate`): `timeline` keeps the oldest buckets and reports `truncated` honestly; narrow with `--since`/`--until`/`--bucket`. Drain-light templates + the `templates`/`rollups`/`event_dims` tables (for million-line logs) are their own later wave with a real-log eval, like JEV — not codegen'd blind. `attr:` uses a bound `json_extract` path plus a `[A-Za-z0-9_.-]` key allowlist (no SQL injection).

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
