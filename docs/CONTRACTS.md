# Contracts: how other tools consume chrono

> 🇪🇸 [Versión en español](CONTRACTS.es.md).

chrono is its own public product (MIT, one binary). Other tools adopt it by
**contract, not by code**: they run the binary and rely on the four things
below. Anything not written here is an implementation detail and may change.

The current consumers are **UltraMemory** (a memory stack for agents, which
serves chrono as one layer behind kindling's MCP gateway) and, through that
gateway, the **kindling Claude Code plugin**. The classifier contract points
the other way: chrono produces data for **Chispa**, kindling's linear
classifier.

Both implementations honour these contracts: the Go binary (what `install.sh`
and the releases ship, from v0.2.0) and the Rust port in `rust/`.

## 1. Repo label: `<name>@<branch>`

Every answer about a repository is tagged with its label:

```
kindling@main
kindling@claude/fix-x
```

- `name` is the directory name of the **main** repository. A linked git
  worktree of `kindling` is labelled `kindling@<branch>`, not with its own
  folder name.
- `branch` is the branch checked out in that tree, read from `.git/HEAD`
  (no `git` process). A detached HEAD is `HEAD`. A directory that is not a git
  repo is labelled with its name alone.
- The branch keeps its `/`. When a label must become a directory name, the
  **slug** replaces every run of characters outside `[A-Za-z0-9._-]` with `-`
  and trims `-`/`.` (UltraMemory's `ramas.py` rule): `claude/fix-x` →
  `claude-fix-x`.

This is the same `repo@branch` convention UltraMemory uses for codegraph and
graphify, so an agent can line answers up across layers.

## 2. Entity key: `path` or `path#symbol`

A file entity is addressed by its **path relative to the repo root**, with
`/` separators: `internal/mcp/mcp.go`. A symbol is `path#symbol`
(`internal/mcp/mcp.go#Serve`); chrono indexes symbols only with
`init --symbols` (Rust port).

Inputs are normalised the same way everywhere a file or path is expected
(`coupling`'s `file`, `owners`' `path`, in the CLI and in MCP):

| Input | Becomes |
| --- | --- |
| `src/a.go#Parse` | `src/a.go` — the `#symbol` part is ignored for file-level answers |
| `./src/a.go` | `src/a.go` |
| `/abs/repo/src/a.go#x` | `src/a.go` — an absolute path inside the repo is made relative |
| `/somewhere/else.go` | unchanged (it is outside the repo) |

So an agent can pass a codegraph id `path#symbol` straight in. The one
exception: in the Rust CLI, `--by symbol` keeps `file#func` intact, because
there the symbol *is* the entity. A file name that really contains `#` cannot
be queried through this normalisation.

## 3. MCP: the optional `repo` argument

One `chrono mcp` process can serve many repositories. Every tool takes an
optional string argument `repo` (like codegraph's `projectPath`):

| `repo` | Resolves to |
| --- | --- |
| *(absent)* | the index found upward from the server's working directory, exactly as before |
| `kindling` | `<root>/kindling` |
| `kindling@feature-x` | first the directory (or symlink) named like the label, `<root>/kindling@feature-x` (a `/` in the branch becomes `-`; the full slug is tried too) — whatever tree it points at; otherwise `<root>/kindling` if it is on that branch, then its linked git worktree on that branch, then `<root>/../ramas/kindling/<slug>` (UltraMemory's worktree layout) |
| `/abs/path/to/tree` | that tree |

- **Root**: `chrono mcp -repos DIR` (also `--repos DIR`) or the `CHRONO_REPOS`
  environment variable; the flag wins. Without a root, `repo` accepts absolute
  paths and the default repo's own name or label.
- **Names** cannot contain `/` or `\`, start with `.`, or be `..`: a name never
  escapes the root. Absolute paths are accepted anywhere the server process
  can read; put the server behind something that authenticates (kindling's
  gateway does) before exposing it.
- **Symlinks are followed.** UltraMemory runs
  `chrono mcp -repos /data/chrono/arboles`, a folder of symlinks `kindling`,
  `kindling@<slug>`… each pointing at a tree with its own index.
- **Where the index lives**: always `<tree>/.chrono/index.db`, created by
  `chrono init <tree>` as today. Serving many repos changes nothing about
  where indexes are; each tree carries its own. (`.chrono/` writes its own
  `.gitignore`; on a mirror you do not commit to, also add `.chrono/` to
  `.git/info/exclude` so `config.json` does not show as untracked.)
- **Go and Rust indexes are not interchangeable** (schema v1 vs v2+). Index
  and serve a root with the same binary.
- Indexes are opened on first use and kept open for the life of the process.

**Every answer carries the label**: the JSON text of each tool result has a
top-level `"repo": "<name>@<branch>"` next to its own fields (for the default
repo, when the index knows its `repo_path`). In the Go binary this means
list-shaped results are now wrapped in an object (`{"hotspots": [...]}`,
`{"churn": [...]}`, `{"phases": [...]}`, `{"pull_requests": [...]}`,
`{"query", "results"}`, `{"of", "similar"}`), the same keys as the CLI.

**Errors are tool results with `isError: true`** and a sentence an agent can
act on:

```
chrono: no chrono index for gamma@main (/data/repos/gamma). Run: chrono init /data/repos/gamma
chrono: repo "kindling@nope": branch "nope" is not checked out (neither /data/repos/kindling nor any of its worktrees is on it)
chrono: repo "x": no repos root configured (set CHRONO_REPOS or run `chrono mcp --repos DIR`), or pass an absolute path
```

(With no `repo` and no index in the working directory, the old behaviour
stays: a plain text answer asking for `chrono init`.)

**The `repos` tool** lists the entries of the root that are git trees or have
an index (symlinks followed, dangling ones skipped), sorted by name:

```json
{ "root": "/data/chrono/arboles", "default": "chrono@main",
  "repos": [
    { "repo": "kindling@main", "name": "kindling", "path": "/data/chrono/arboles/kindling", "indexed": true },
    { "repo": "kindling@claude/fix-x", "name": "kindling@claude-fix-x", "path": "/data/chrono/arboles/kindling@claude-fix-x", "indexed": true },
    { "repo": "notes@main", "name": "notes", "path": "/data/chrono/arboles/notes", "indexed": false }
  ] }
```

`name` is what you pass back as `repo`; `repo` is the label answers will carry.

## 4. Dataset for Chispa: `chrono export-dataset`

chrono does not train or embed a classifier of its own. The learned level is
**Chispa** (kindling: `kling ai chispa train|eval|predict|deploy`); chrono
exports labelled commits in the JSONL that `kling ai chispa train` reads:

```sh
chrono export-dataset --out commits.jsonl        # or stdout; --since DATE, --body N (default 300)
kling ai chispa train -data commits.jsonl -o commits.chispa
kling ai chispa eval  -model commits.chispa -data held-out.jsonl
```

One line per commit:

```json
{"text":"crash on empty cache","label":"fix","fields":{"files":1,"churn":7,"ext":[".go"],"dir":["cache"]},"id":"<sha>","repo":"kindling@main","time":1738368000,"label_source":"forge"}
```

- `text`: the subject **with the conventional-commit prefix removed** (`fix(api)!:`
  is the label; leaving it in teaches the model to read four letters), then,
  on a new line, up to `--body` bytes of the body without trailers
  (`Signed-off-by`, `Co-authored-by`, `Refs`…) or lines that start with a
  prefix (squash merges list them: label leak). Cut on a UTF-8 boundary.
- `label`: chrono's deterministic signal, first match wins: a bug label on the
  forge (`label_source: "forge"`), a revert, the conventional prefix, then a
  `fix_keywords` hit (`"rules"`). Commits with no signal are not exported.
- `fields`: `files` (count), `churn` (lines added+deleted, binaries 0), `ext`
  and `dir` (up to 8 extensions and first-level directories, most frequent
  first, ties by name) — the same fields as kindling's `tools/chispa-commits`.
- `id`, `repo`, `time` (epoch seconds), `label_source`: extra keys Chispa
  ignores when reading; they let you split and audit.
- Order is oldest first, so a leak-free temporal split is `head`/`tail`.
  Repeated texts (case and whitespace folded) keep their first occurrence.

Verified: on OpenWA (1,738 commits) both binaries export the same 1,726 rows
and `kling ai chispa train` reads the file unchanged.

**Next step, not implemented:** with `CHRONO_CHISPA_MODEL=<m.chispa>`, `init`
and `sync` would send commits that have no hard signal to
`kling ai chispa predict -model M` on stdin (JSONL in, one JSON answer per
line) and keep a label only when `decision` is `confident`, with
`source: "chispa"`. Rules stay the default and the fallback: no `kling`, no
model, or `escalate` leaves the rules' answer.

## How UltraMemory and the kindling plugin use this

1. UltraMemory keeps its trees (mirrors under `/data/repos`, branch worktrees
   under `/data/ramas/<repo>/<slug>`) and runs `chrono init` / `chrono sync`
   on each, so each tree has its `.chrono/index.db`.
2. It points one server at a root of symlinks:
   `chrono mcp -repos /data/chrono/arboles`, wrapped by `mcp-proxy` (stdio →
   HTTP) and registered in kindling's gateway with `kling mcp link`.
3. Agents reach it through the gateway. With the kindling Claude Code plugin,
   Claude uses the gateway's `find_tools` / `describe_tool` / `call_tool`, e.g.
   `call_tool` on chrono's `coupling` with
   `{"repo": "kindling@main", "file": "pkg/aigw/gateway.go#Serve"}`, and
   joins the answer with codegraph and graphify by the label and the entity
   key.
4. `repos` tells the agent which labels exist and which still need
   `chrono init`.
5. For classification, `chrono export-dataset` feeds `kling ai chispa train`;
   the evaluation that decides whether a model is worth using is
   `kling ai chispa eval`, not chrono's.
