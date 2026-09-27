# Changelog

All notable changes to chrono. The Go binary is what the releases and
`install.sh` ship; the Rust port in `rust/` follows the same contracts.

## v0.2.0 — unreleased

Integration with UltraMemory and kindling **by contract** (see
[`docs/CONTRACTS.md`](docs/CONTRACTS.md)).

### Added

- **Multi-repo MCP.** Every MCP tool takes an optional `repo` argument: a name
  under the repos root (`chrono mcp -repos DIR`, `--repos DIR` or
  `CHRONO_REPOS`), a `<name>@<branch>` label, or an absolute path. A label
  resolves first to a directory or symlink named like it
  (`<root>/kindling@feature-x`), then to the named repo on that branch, its git
  worktree, or `../ramas/<name>/<slug>`. Without `repo` nothing changes.
- **`repos` MCP tool**: the trees under the root (symlinks followed, dangling
  ones skipped), their label and whether they have an index.
- **Repo label in every MCP answer**: `"repo": "<name>@<branch>"`.
- **Clear errors** for a repo without index (`Run: chrono init <path>`), a
  branch that is not checked out, an unknown name, or a missing root, as tool
  results with `isError: true`.
- **Shared entity key**: `coupling`/`owners` (CLI and MCP) accept
  `path#symbol` (the symbol is ignored for file-level answers), `./path` and
  absolute paths inside the repo, so codegraph ids can be passed straight in.
- **`chrono export-dataset`** (`--out`, `--since`, `--body`): labelled commits
  as JSONL `{"text","label","fields"}` in the exact format of kindling's
  `kling ai chispa train`, conventional prefix removed from the text,
  oldest first, duplicates dropped. Go and Rust produce the same file.
- `docs/CONTRACTS.md` (+ Spanish version): repo label, entity key, MCP `repo`
  argument, dataset format, and how UltraMemory and the kindling Claude Code
  plugin consume chrono.

### Changed

- **MCP results of the Go binary are always objects**, with the same keys as
  the CLI: `hotspots`, `churn`, `phases`, `prs` (`pull_requests`), `search`
  (`query`, `results`) and `similar` (`of`, `similar`) used to be bare lists.
  `owners` also returns the normalised `path`.
- MCP `initialize` reports the binary's real version instead of `0.1.0`.
- The learned-classifier plan is now **Chispa** (kindling); chrono will not
  embed a classifier of its own. Docs and code comments renamed accordingly
  (DECISIONS #21, DESIGN-GENERAL-CORE §5).
- Version 0.2.0 in the Makefile and `flake.nix`.

### Not yet

- Querying a trained `.chispa` at ingest (`CHRONO_CHISPA_MODEL`, via
  `kling ai chispa predict`) is designed in `docs/CONTRACTS.md` but not
  implemented; rules remain the only classifier.

## v0.1.1

- `chrono branches [base]` (also over MCP); precise warnings when the forge is
  skipped; actionable error when `git` is missing.

## v0.1.0

- First release: `init`/`sync`, hotspots, coupling, owners, bugs, churn,
  tickets, prs, phases, search, similar, MCP over stdio; one-line installer,
  Nix flake, English/Spanish messages.
