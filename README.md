# chrono

> 🇪🇸 ¿Prefieres español? Lee el [README en español](README.es.md).

**Turn a repo's Git history into answers.** chrono reads your commits, changes, authors and PRs once, stores them in a local index, and answers questions like *"what's dangerous to touch?"*, *"what breaks if I change this file?"* or *"who owns this part of the code?"* — as JSON, instantly, without you (or an AI) having to read thousands of commits.

One binary. No server, no daemon. Runs on your Mac or any Linux.

```console
$ cd my-repo
$ chrono init            # index the repo (once)
$ chrono hotspots        # which files are a minefield?
$ chrono coupling src/auth.go   # what always changes together with this?
$ chrono bugs            # where do fixes concentrate, and what kind?
```

---

## Requirements

| Tool | Required? | For |
| --- | --- | --- |
| **git** | Yes | reading history (you already have it if you have repos) |
| **gh** ([cli.github.com](https://cli.github.com)) | Optional | reading GitHub PRs & issues (`chrono prs`, tickets) |
| **Go 1.23+** | Build only | not needed if you use a prebuilt binary |

chrono needs **no database, no server, no network** (except `gh` for PRs). The index is a local file.

---

## Install

### Option A — One line (recommended, macOS & Linux)

Detects your platform, downloads the matching binary from the latest release, verifies its SHA-256, and installs it to a directory on your `PATH`:

```bash
curl -fsSL https://raw.githubusercontent.com/juan52878911/chrono/main/install.sh | sh
```

No clone, no Go, no compiler. Works on macOS (Apple Silicon & Intel) and Linux (x86-64 & arm64).

- Pin a version: `curl -fsSL https://raw.githubusercontent.com/juan52878911/chrono/main/install.sh | VERSION=v0.1.0 sh`
- Choose the install dir: `... | PREFIX="$HOME/.local/bin" sh`
- By default it installs to `/usr/local/bin` when writable, otherwise `~/.local/bin` (the script prints a PATH hint if needed).

### Option B — Prebuilt binary (manual)

Download the `.tar.gz` for your platform from the [Releases page](https://github.com/juan52878911/chrono/releases/latest) and put it on your `PATH`:

**macOS (Apple Silicon / M1–M4):**
```bash
tar -xzf chrono-v0.1.0-darwin-arm64.tar.gz
sudo mv chrono-v0.1.0-darwin-arm64/chrono /usr/local/bin/
```
**macOS (Intel):** use `...-darwin-amd64.tar.gz`.

**Linux x86-64 (Debian/Ubuntu, Arch, …):**
```bash
tar -xzf chrono-v0.1.0-linux-amd64.tar.gz
sudo install -m755 chrono-v0.1.0-linux-amd64/chrono /usr/local/bin/chrono
```
> The Linux binary is **static** (no system dependencies): the same file works on Debian, Ubuntu, Arch, Alpine, etc.

From a checkout you can also just run `./install.sh` (same script, uses `./dist` if present).

### Option C — From source (with Go)
```bash
make install     # installs to ~/go/bin
# or:  go install github.com/juan52878911/chrono/cmd/chrono@latest
```

### Option D — Nix
```bash
nix run   github:juan52878911/chrono          # run without installing
nix profile install github:juan52878911/chrono # install into your profile
```
Or as an input in your `flake.nix`:
```nix
inputs.chrono.url = "github:juan52878911/chrono";
# ... environment.systemPackages = [ inputs.chrono.packages.${system}.default ];
```
The Nix package wraps `git` and `gh` automatically.

### Verify
```bash
chrono version        # -> chrono v0.1.0
```

---

## 30-second tour

```bash
cd your-repo
chrono init                       # creates .chrono/ and indexes (git-ignored)
chrono hotspots                   # files that change most and weigh most
chrono coupling path/to/file      # what changes together with it
chrono owners backend/            # ownership by author + bus factor
chrono bugs --since 2025-01-01    # bug categories + hot areas, in a window
chrono search "webhook handling"  # search commits by meaning
chrono sync                       # update only what's new (very fast)
```

Every query accepts `--json` (default) and `--since DATE`. The index is
**auto-discovered** by walking up from the current directory, like git with `.git`.

---

## Commands

| Command | Answers |
| --- | --- |
| `init [repo]` | Build the index (creates `.chrono/`) and ingest everything. |
| `sync [repo]` | Process only the delta since last time. |
| `hotspots` | What's dangerous to touch? (frequency × size) |
| `coupling <file>` | What breaks if I touch this? (temporal coupling) |
| `owners <path>` | Ownership by author and **bus factor**. |
| `bugs` | Where fixes concentrate + **bug categories** (semantic). |
| `churn` | Lines +/− per file. |
| `tickets <id>` | Commits, files and PRs for a ticket. |
| `prs` | Forge pull requests (state, merge, bug by label). |
| `phases` | Project phases (tags/releases). |
| `search <text>` | Search commits by meaning (full-text). |
| `similar <sha>` | Near-duplicate commits (by SimHash fingerprint). |
| `mcp` | MCP server over stdio (for an AI). |

Options: `--db PATH`, `--since DATE`, `--lang en|es`.

---

## With an AI (opencode / Claude Code)

chrono speaks **MCP**. An AI can answer questions about your history by reading
~1,000-token summaries instead of thousands of commits.

**opencode** (`~/.config/opencode/opencode.json`):
```json
{ "mcp": { "chrono": { "type": "local", "command": ["chrono", "mcp"], "enabled": true } } }
```
**Claude Code** (project `.mcp.json`):
```json
{ "mcpServers": { "chrono": { "command": "chrono", "args": ["mcp"] } } }
```
The server starts on demand and dies with the session (serverless). It auto-discovers the index from the working directory. If a repo has no index yet, the tools reply *"run chrono init"* instead of failing.

---

## Configuration (optional)

`chrono init` creates `.chrono/config.json`, versionable and tunable per repo:

```json
{
  "fix_keywords": ["fix", "bug", "arregl", "corrig", "..."],
  "ticket_patterns": ["\\b([A-Z][A-Z0-9]+-\\d+)\\b", "(#\\d+)"],
  "exclude_globs": ["*-lock.json", "dist/*", "..."],
  "bug_labels": ["bug", "defect", "regression"],
  "bug_categories": { "memory-safety": ["leak", "use-after-free", "..."], "network": ["tls", "socket", "..."] }
}
```
- **fix_keywords**: which words (in your language) mark a fix.
- **ticket_patterns**: what your tickets look like (Jira `PROJ-123`, GitHub `#45`).
- **exclude_globs**: generated files you don't want in hotspots/churn.
- **bug_labels**: PR/issue labels that count as a bug (deterministic forge signal).
- **bug_categories**: taxonomy `category → keywords` used to classify fixes.

---

## Language

Messages are **English by default**. chrono switches to Spanish when your OS locale
is Spanish (`LANG`/`LC_*`), or with `CHRONO_LANG=es`, or `--lang es`. JSON output is
never translated (it's the contract).

---

## How it works (and why to trust it)

- **Deterministic:** answers come from SQL over a fixed index → the same question gives the same answer every time, with a reproducible manifest (git version, thresholds…). An AI reading logs does not.
- **Bounded:** every answer is a small JSON with a token budget; it never dumps raw commits.
- **Efficient:** ~6.7 MB binary; a few-MB index (e.g. 876 commits ≈ 1.5 MB, 17k commits ≈ tens of MB); incremental `sync` in milliseconds; no background processes.
- **Local and private:** everything lives in `.chrono/` inside your repo. The forge (`gh`) is only queried when available (and it prefers your `upstream` remote on forks).

More detail in [`docs/`](docs/): architecture, output contract, decisions and roadmap.

---

## License

MIT — see [LICENSE](LICENSE).
