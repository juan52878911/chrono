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

**Release:** `make release` builds macOS (arm64/Intel) + Linux (x86-64/arm64) tarballs + `SHA256SUMS`; `flake.nix` builds with Nix (wraps git+gh); `install.sh` + bilingual README.

Full commands: init, sync, hotspots, coupling, owners, bugs, churn, tickets, prs, phases, search, similar, mcp.

> Decision point: does it help in real use? If yes → Part B.

---

## PART B — Scale (only if Part A proves valuable)

- **Phase 5 · Dense semantics** — `sqlite-vec` + embeddings via an **external provider** (Ollama / downloadable sidecar / API), int8 blobs, brute-force search in Go. Never embed the model/runtime in the binary.
- **Phase 6 · Trained Level-1 classifier** — only once a labeled dataset + eval metric exist.
- **Phase 7 · `chrono serve`** — daemon: network HTTP/MCP API, auto-sync/webhooks, multi-repo, auth. Deploy in a Proxmox LXC.
- **Phase 8 · Integrations** — GitLab/Jira adapters (on demand), cross-repo, export to Obsidian/Markdown.

---

## Future improvements (value/effort, best first)
1. Time-series churn/fixes per directory.
2. commit→issue→close link (bug lifetime by area).
3. Revert / "fix of the fix" as a fragility signal.
4. Incremental blame **only** on hotspot files.
5. Cross-repo in `serve`.
6. Export summaries to Obsidian/Markdown.
