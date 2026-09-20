// Command chrono extrae conocimiento del historial de Git a un índice SQLite
// consultable, y responde preguntas acotadas (JSON) sin que la IA lea commits
// crudos.
package main

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/juan52878911/chrono/internal/config"
	"github.com/juan52878911/chrono/internal/i18n"
	"github.com/juan52878911/chrono/internal/ingest"
	"github.com/juan52878911/chrono/internal/mcp"
	"github.com/juan52878911/chrono/internal/metrics"
	"github.com/juan52878911/chrono/internal/report"
	"github.com/juan52878911/chrono/internal/store"
)

var version = "dev"

const usageEN = `chrono %s — Git history knowledge, bounded for an AI.

Usage:
  chrono init [repo-path]   Build the repo index (creates .chrono/) and ingest.
  chrono <query> [args]     Auto-discovers .chrono/index.db upward from cwd.

Index:
  init [repo]          Zero-config: detect the repo, create .chrono/, ingest all.
  sync [repo]          Process only the delta since the last watermark.

Queries (JSON, see docs/OUTPUT-CONTRACT.md):
  hotspots             Files that change most and weigh most.
  coupling <file>      What changes together with this file.
  owners <path>        Ownership by author and bus factor.
  bugs                 Where fixes concentrate + bug categories.
  churn                Lines +/- per file.
  tickets <id>         Commits/files/PRs linked to a ticket.
  prs                  Forge pull requests (state, merge, bug by label).
  branches [base]      Branch status vs base: ahead/behind, merged, stale, authors.
  phases               Project phases (tags/releases).
  search <text>        Search commits by meaning (FTS5, LIKE fallback).
  similar <sha>        Near-duplicate commits (by SimHash).

Integration:
  mcp                  MCP server over stdio (for opencode / Claude). Serverless.

Options:
  --db PATH            Explicit index (otherwise auto-discovers .chrono/index.db).
  --since DATE         Time window (e.g. 2025-01-01).
  --lang en|es         Message language (default: auto from OS locale).

Other: version, help
`

const usageES = `chrono %s — conocimiento del historial de Git, acotado para una IA.

Uso:
  chrono init [ruta-repo]   Prepara el índice del repo (crea .chrono/) e ingiere.
  chrono <consulta> [args]  Auto-descubre .chrono/index.db subiendo desde el cwd.

Índice:
  init [repo]          Cero-config: detecta el repo, crea .chrono/ e ingiere todo.
  sync [repo]          Procesa solo el delta desde la última marca de agua.

Consulta (JSON según docs/OUTPUT-CONTRACT.md):
  hotspots             Ficheros que más cambian y más pesan.
  coupling <fichero>   Qué cambia junto a este fichero.
  owners <ruta>        Propiedad por autor y bus factor.
  bugs                 Dónde se concentran los fixes + categorías.
  churn                Líneas +/- por fichero.
  tickets <id>         Commits/ficheros/PRs ligados a un ticket.
  prs                  PRs del forge (estado, merge, si es bug por label).
  branches [base]      Estado de ramas vs base: ahead/behind, mergeada, stale, autores.
  phases               Fases del proyecto (etiquetas).
  search <texto>       Busca commits por significado (FTS5, con fallback a LIKE).
  similar <sha>        Commits casi-duplicados (por SimHash).

Integración:
  mcp                  Servidor MCP por stdio (para opencode / Claude). Serverless.

Opciones:
  --db RUTA            Índice explícito (si no, se auto-descubre .chrono/index.db).
  --since FECHA        Ventana temporal (p.ej. 2025-01-01).
  --lang en|es         Idioma de los mensajes (por defecto: auto según el SO).

Otros: version, help
`

func usage() string { return i18n.T(usageEN, usageES) }

const indexRel = ".chrono/index.db"

type flags struct {
	db    string
	since string
	lang  string
	pos   string
}

// parseArgs saca el comando (1er positional) y su argumento (2º), aceptando
// flags en cualquier posición: `chrono --lang es hotspots` o `chrono hotspots --since X`.
func parseArgs(args []string) (cmd string, f flags) {
	var pos []string
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--db":
			if i+1 < len(args) {
				i++
				f.db = args[i]
			}
		case "--since":
			if i+1 < len(args) {
				i++
				f.since = args[i]
			}
		case "--lang":
			if i+1 < len(args) {
				i++
				f.lang = args[i]
			}
		default:
			if strings.HasPrefix(args[i], "--") {
				continue
			}
			pos = append(pos, args[i])
		}
	}
	if len(pos) > 0 {
		cmd = pos[0]
	}
	if len(pos) > 1 {
		f.pos = pos[1]
	}
	return cmd, f
}

func main() {
	cmd, f := parseArgs(os.Args[1:])
	i18n.Set(f.lang) // --lang gana sobre la detección por SO.
	if cmd == "" {
		fmt.Fprintf(os.Stderr, usage(), version)
		os.Exit(2)
	}

	switch cmd {
	case "version", "--version", "-v":
		fmt.Printf("chrono %s\n", version)
		return
	case "help", "--help", "-h":
		fmt.Printf(usage(), version)
		return
	case "init":
		cmdInit(f)
		return
	case "mcp":
		// El servidor MCP NO debe morir sin índice: arranca igual y cada tool
		// responde "ejecuta chrono init". Así opencode lo ve conectado.
		runMCP(f)
		return
	}

	// Comandos que necesitan un índice existente.
	db := resolveDB(f, cmd)
	st, err := store.Open(db)
	if err != nil {
		fatal(err)
	}
	defer st.Close()
	if err := st.Migrate(); err != nil {
		fatal(err)
	}

	switch cmd {
	case "sync":
		requireGit()
		repo := f.pos
		if repo == "" {
			if rp, ok, _ := st.Meta("repo_path"); ok {
				repo = rp
			}
		}
		requirePos(repo, i18n.T("don't know which repo to sync (no argument and no repo_path in the index)", "no sé qué repo sincronizar (ni argumento ni repo_path en el índice)"))
		n, err := ingest.Sync(repo, st)
		if err != nil {
			fatal(err)
		}
		fmt.Fprintf(os.Stderr, i18n.T("sync: %d new commits\n", "sync: %d commits nuevos\n"), n)
	case "hotspots":
		res, err := metrics.Hotspots(st.DB(), f.since, 25)
		emit(st, "hotspots", f.since, 25, len(res), map[string]any{"hotspots": res}, err)
	case "coupling":
		requirePos(f.pos, i18n.T("coupling needs a <file>", "coupling necesita un <fichero>"))
		pairs, _, err := metrics.Coupling(st.DB(), f.pos, 2, f.since, 25)
		emit(st, "coupling", f.since, 25, len(pairs), map[string]any{"for": f.pos, "coupled": pairs}, err)
	case "owners":
		requirePos(f.pos, i18n.T("owners needs a <path>", "owners necesita una <ruta>"))
		owners, bf, err := metrics.Owners(st.DB(), f.pos, f.since)
		emit(st, "owners", f.since, len(owners), len(owners), map[string]any{"owners": owners, "bus_factor": bf}, err)
	case "bugs":
		res, err := metrics.CommonBugs(st.DB(), bugTaxonomy(st), f.since, 15)
		emit(st, "bugs", f.since, 15, 15, res, err)
	case "churn":
		res, err := metrics.Churn(st.DB(), f.since, 25)
		emit(st, "churn", f.since, 25, len(res), map[string]any{"churn": res}, err)
	case "tickets":
		requirePos(f.pos, i18n.T("tickets needs an <id>", "tickets necesita un <id>"))
		res, err := metrics.Ticket(st.DB(), f.pos)
		emit(st, "tickets", "", 1, 1, res, err)
	case "branches":
		requireGit()
		rp, ok, _ := st.Meta("repo_path")
		if !ok || rp == "" {
			fatal(fmt.Errorf("%s", i18n.T("no repo_path in the index; run 'chrono init' first", "no hay repo_path en el índice; ejecuta 'chrono init' primero")))
		}
		base, cur, res, err := metrics.Branches(rp, f.pos)
		emit(st, "branches", "", len(res), len(res), map[string]any{"base": base, "current": cur, "branches": res}, err)
	case "phases":
		res, err := metrics.Phases(st.DB())
		emit(st, "phases", "", len(res), len(res), map[string]any{"phases": res}, err)
	case "prs":
		res, err := metrics.PullRequests(st.DB(), f.since, 30)
		emit(st, "prs", f.since, 30, len(res), map[string]any{"pull_requests": res}, err)
	case "search":
		requirePos(f.pos, i18n.T("search needs <text>", "search necesita un <texto>"))
		res, err := metrics.Search(st.DB(), st.HasFTS(), f.pos, 25)
		emit(st, "search", "", 25, len(res), map[string]any{"query": f.pos, "results": res}, err)
	case "similar":
		requirePos(f.pos, i18n.T("similar needs a <sha>", "similar necesita un <sha>"))
		res, err := metrics.Similar(st.DB(), f.pos, 4, 15)
		emit(st, "similar", "", 15, len(res), map[string]any{"of": f.pos, "similar": res}, err)
	default:
		fmt.Fprintf(os.Stderr, i18n.T("chrono: unknown command %q\n\n", "chrono: comando desconocido %q\n\n")+usage(), cmd, version)
		os.Exit(2)
	}
}

// runMCP arranca el servidor MCP. Si hay índice lo abre; si no, sirve igual
// (los tools responderán pidiendo `chrono init`). Nunca muere por falta de índice.
func runMCP(f flags) {
	var st *store.Store
	p := f.db
	if p == "" {
		if d, ok := discoverDB(); ok {
			p = d
		}
	}
	if p != "" {
		if s, err := store.Open(p); err == nil {
			if err := s.Migrate(); err == nil {
				st = s
				defer s.Close()
			}
		}
	}
	if err := mcp.Serve(st); err != nil {
		fatal(err)
	}
}

// cmdInit prepara el índice del repo actual (o el indicado) sin más ceremonia.
func cmdInit(f flags) {
	requireGit()
	start := f.pos
	if start == "" {
		start, _ = os.Getwd()
	}
	root, err := repoRoot(start)
	if err != nil {
		fatal(fmt.Errorf(i18n.T("%s is not inside a git repo", "%s no está dentro de un repo git"), start))
	}
	dir := filepath.Join(root, ".chrono")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		fatal(err)
	}
	// Todo .chrono/ se ignora en git salvo la config (esa sí conviene versionarla).
	_ = os.WriteFile(filepath.Join(dir, ".gitignore"), []byte("*\n!config.json\n"), 0o644)
	if err := config.WriteDefault(dir); err != nil {
		fmt.Fprintln(os.Stderr, "chrono: aviso: no se pudo escribir config.json:", err)
	}

	db := filepath.Join(dir, "index.db")
	st, err := store.Open(db)
	if err != nil {
		fatal(err)
	}
	defer st.Close()
	if err := st.Migrate(); err != nil {
		fatal(err)
	}
	if err := st.SetMeta("repo_path", root); err != nil {
		fatal(err)
	}
	n, err := ingest.Run(root, st, true)
	if err != nil {
		fatal(err)
	}
	rel, _ := filepath.Rel(root, db)
	fmt.Fprintf(os.Stderr, i18n.T("chrono: index ready at %s (%d commits).\n", "chrono: índice listo en %s (%d commits).\n"), rel, n)
	fmt.Fprintln(os.Stderr, i18n.T("Now, inside the repo: chrono hotspots · chrono bugs · chrono sync", "Ahora, dentro del repo: chrono hotspots · chrono bugs · chrono sync"))
}

// resolveDB decide qué índice usar: --db explícito, o auto-descubierto.
func resolveDB(f flags, cmd string) string {
	if f.db != "" {
		return f.db
	}
	if p, ok := discoverDB(); ok {
		return p
	}
	fatal(fmt.Errorf("%s", i18n.T(
		"no index (.chrono/index.db) found upward from here.\nRun 'chrono init' inside the repo, or pass --db PATH",
		"no encontré un índice (.chrono/index.db) subiendo desde aquí.\nEjecuta 'chrono init' dentro del repo, o pasa --db RUTA")))
	return ""
}

// discoverDB sube desde el cwd buscando .chrono/index.db (como git con .git).
func discoverDB() (string, bool) {
	dir, err := os.Getwd()
	if err != nil {
		return "", false
	}
	for {
		p := filepath.Join(dir, indexRel)
		if fi, err := os.Stat(p); err == nil && !fi.IsDir() {
			return p, true
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			return "", false
		}
		dir = parent
	}
}

func repoRoot(path string) (string, error) {
	out, err := exec.Command("git", "-C", path, "rev-parse", "--show-toplevel").Output()
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(string(out)), nil
}

// bugTaxonomy carga la taxonomía de categorías desde la config del repo (o defaults).
func bugTaxonomy(st *store.Store) map[string][]string {
	rp, _, _ := st.Meta("repo_path")
	return config.Load(rp).BugCategories
}

func emit(st *store.Store, question, since string, maxItems, returned int, result any, err error) {
	if err != nil {
		fatal(err)
	}
	env := report.New(st, question, since, maxItems, returned, result)
	if err := report.PrintJSON(env); err != nil {
		fatal(err)
	}
}

func requirePos(p, msg string) {
	if p == "" {
		fatal(fmt.Errorf("%s", msg))
	}
}

// requireGit aborta con un mensaje accionable si git no está instalado. Solo
// init/sync lo necesitan; las consultas leen el índice y funcionan sin git.
func requireGit() {
	if _, err := exec.LookPath("git"); err != nil {
		fatal(fmt.Errorf("%s", i18n.T(
			"git is not installed, and chrono needs it to read history.\n  Install it: https://git-scm.com/downloads  (macOS: xcode-select --install, or brew install git)",
			"git no está instalado y chrono lo necesita para leer el historial.\n  Instálalo: https://git-scm.com/downloads  (macOS: xcode-select --install, o brew install git)")))
	}
}

func fatal(err error) {
	fmt.Fprintln(os.Stderr, "chrono: error:", err)
	os.Exit(1)
}
