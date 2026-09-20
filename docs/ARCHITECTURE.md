# Architecture

## Principle: hexagonal, without extra ceremony

The core (`internal/domain`) speaks in domain terms (Revision, Author, Change, metrics) and does **no I/O**. Adapters at the edge translate. But a port (interface) is only created where there will genuinely be **two or more** implementations — don't over-abstract:

| Port | Interface in v1? | Why |
| --- | --- | --- |
| **Source** (history) | Yes | git today; GitLab/others on the roadmap → a real second adapter. |
| **Tracker** (tickets/PRs) | Yes | GitHub today; Jira/Linear committed. |
| **Classifier** | Yes | real implementations: rules → local → Jev/LLM. |
| **Store** | No (direct call) | only SQLite for now. Add the interface the day of the 2nd backend. |
| **Embedder** | No (arrives in v2) | only local ONNX planned. |

## Storage: a single SQLite file

- **Relational + graph**: the author·commit·file·PR graph is tables + recursive CTEs. No Memgraph.
- **Full-text search**: FTS5 (bundled in modernc) powers `chrono search`; falls back to `LIKE` if unavailable.
- **Near-duplicates**: a 64-bit SimHash per commit (8 bytes) powers `chrono similar`.
- **Vectors**: `sqlite-vec` in-process is a **v2** option (dense embeddings), not needed today.
- WAL enabled: reads during `sync` (single writer). `VACUUM` + checkpoint on finish keep the `.db` minimal with no lingering `-wal`.
- Versioned schema (`meta.schema_version`); see migration in `DECISIONS.md`.

## Ingest pipeline

```
git log --numstat        (streaming, ~constant RAM, progress every 2000 commits)
   → parser              (folds into aggregates)
   → normalize authors   (mailmap + identities table)
   → SimHash + classify  (rules: fix/feat/…, revert, ticket refs)
   → SQLite              (graph + labels + FTS + simhash)
```

- **First pass = batch** (honest: seconds to a minute on large repos; ~37s for 17k commits).
- **`sync` = only the delta** since `meta.last_sha`. If that SHA no longer exists (rebase/force-push) → rebuild, not "subtract".
- Rejects **shallow** clones (metrics would be false).
- Commits touching more than `BulkThreshold` (50) files are marked `is_bulk` and excluded from coupling.
- **File sizes** are computed only for the top ~4000 most-changed files (what hotspots needs), cached by blob OID — so `sync` reads only new blobs.

## Forge (Tracker)

- Uses `gh`; prefers the **`upstream`** remote when present (fork workflow — PRs live upstream), else `origin`.
- Fetches PRs/issues without heavy fields (`body`/`commits`) to stay under GitHub's GraphQL node limit; 90s timeout; surfaces `gh` errors instead of failing silently.
- Bug labels are the **deterministic** signal for "is this a bug": a commit linked to a bug-labeled PR/issue is marked a fix with confidence 1.0.

## Runtimes

- **v1: CLI + MCP stdio.** No daemon. The MCP server starts even without an index (its tools reply "run chrono init").
- **Scaling: `chrono serve`** (network API, auto-sync/webhooks, multi-repo, auth) — effectively a second product, in a later phase.

## "Most common bugs": relational + taxonomy

The answer combines **where** (relational: directories with the most fixes, weighted by recency) and **what kind** (a configurable keyword **taxonomy** → semantic categories like memory-safety, crash, network, install-deps). Categories, their top files and example SHAs come out deterministically, without a model.

## Language

User messages are English by default, Spanish via OS locale / `--lang` / `CHRONO_LANG`. JSON output is never translated — it is the API contract.
