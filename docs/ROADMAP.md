# Roadmap — personal first, then scale

Strategy: **make it usable for personal use, test it, and only if it works well, scale.** Estimates are dev-days at part-time.

---

## PART A — Usable for personal use (done)

- **Phase 0 · Skeleton + contract** ✅ — repo layout, SQLite schema, output contract, catalog, manifest.
- **Phase 1 · Deterministic ingest** ✅ — streaming `git log --numstat`, shallow rejection, divergence detection, incremental `sync`, renames/deletes/binaries, author identities.
- **Phase 2 · Relational metrics + JSON CLI** ✅ — hotspots, churn, coupling (with caps), owners, bus factor, `--since` window.
- **Phase 3 · Classification + extraction** ✅ — Level 0 rules (configurable), revert detection, ticket refs, `bugs` (relational + **taxonomy categories**), `phases`.
- **Phase 4 · MCP stdio** ✅ — the same queries exposed to an AI; starts even without an index.

**Brought forward from Part B:** the **Tracker port** (GitHub PRs/issues via `gh`, preferring `upstream`) — `chrono prs`, enriched `tickets`, forge-label determinism.

**Optimizations applied:** binary 10→6.7 MB (`-s -w -trimpath`), `VACUUM`+checkpoint, aggressive ingest PRAGMAs, `size_lines` cached by blob OID + capped to the top ~4000 changed files (huge-repo speed), and **Level-0 semantics with 0 MB binary bloat**: **FTS5** (`search`) + **SimHash** (`similar`). Progress indicator during `init`. English-default i18n (`--lang`, locale).

**Release:** `make release` builds macOS (arm64/Intel) + Linux (x86-64/arm64) tarballs + `SHA256SUMS`; `flake.nix` builds with Nix (wraps git+gh); one-line `curl | sh` installer (downloads from the release + verifies SHA-256) + bilingual README.

**Branch status (v0.1.1)** ✅ — `chrono branches [base]`: per local branch, tip/author/age, **ahead/behind vs base**, merged, stale (>90 days), and authors + bus factor of the branch-unique commits. Read **live from git** (not the index), so no reindex needed. Also exposed over MCP.

**Dependency UX (v0.1.1)** ✅ — `init` warns precisely when the forge is skipped (no `gh` / no GitHub remote / `gh` not authenticated) instead of skipping silently; `init`/`sync` give an actionable error if `git` is missing. Queries still work without `git` (they only read the SQLite index).

Full commands: init, sync, hotspots, coupling, owners, bugs, churn, tickets, prs, branches, phases, search, similar, mcp.

> Decision point: does it help in real use? If yes → Part B.

---

## What we have vs what's pending (honest state)

**We have (measured on bun: 17,678 commits / 19,726 files):**
- Commits & file-changes indexed with **ground-truth accuracy** vs raw git (17,678 = 17,678; per-file counts identical; top-15 hotspots match).
- Init ~42 s, incremental `sync` ~0.05 s, queries <0.5 s, index ~15× smaller than `.git`.
- Branch status live; graceful degradation without `gh`; portable index (query without `git`).

**Pending / known limits (best first):**
1. **Forge capped at 1,000 PRs + 1,000 issues** (`tracker.Fetch(repo, 1000)`). On huge repos (bun has tens of thousands) only the most recent are kept — limits `prs` and bug-by-label counts. → paginate `gh` up to a configurable N.
2. **Branch *relating* (Level B)** — the index still only knows `HEAD`. `branches` reads git live but chrono does **not** tag commits with the branch(es) that contain them, so there's no `chrono branch <name>` (commits/files/hotspots unique to a branch) yet. → ingest with `--all` + `branches`/`commit_branches` tables + schema migration.
3. **`size_lines` capped to top ~4,000 changed files** — files below that rank keep `size_lines=0`. Verified to **not** affect hotspot ranking (those files never rank), but documented for transparency.
4. **Local branches only** — `branches` reads `refs/heads`, not `refs/remotes`. → optional `--remotes` flag.

---

## PART B — Scale (only if Part A proves valuable)

- **Phase 5 · Dense semantics** — `sqlite-vec` + embeddings via an **external provider** (Ollama / downloadable sidecar / API), int8 blobs, brute-force search in Go. Never embed the model/runtime in the binary.
- **Phase 6 · Trained Level-1 classifier** — only once a labeled dataset + eval metric exist.
- **Phase 7 · `chrono serve`** — daemon: network HTTP/MCP API, auto-sync/webhooks, multi-repo, auth. Deploy in a Proxmox LXC.
- **Phase 8 · Integrations** — GitLab/Jira adapters (on demand), cross-repo, export to Obsidian/Markdown.
- **Phase 9 · Branch graph (Level B)** — ingest `--all`, tag commits with containing branches (`branches` + `commit_branches`), `chrono branch <name>` for branch-unique commits/files/hotspots and divergence from base. Schema migration.

---

## Future improvements (value/effort, best first)
1. Time-series churn/fixes per directory.
2. commit→issue→close link (bug lifetime by area).
3. Revert / "fix of the fix" as a fragility signal.
4. Incremental blame **only** on hotspot files.
5. Cross-repo in `serve`.
6. Export summaries to Obsidian/Markdown.
