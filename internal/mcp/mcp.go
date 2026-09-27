// Package mcp expone las consultas de chrono como un servidor MCP por stdio
// (JSON-RPC 2.0 delimitado por saltos de línea). Lo lanza el cliente (Claude)
// y muere con la sesión: no es un daemon.
//
// Multi-repo (docs/CONTRACTS.md): cada tool acepta `repo` opcional — un
// nombre bajo la raíz de repos (CHRONO_REPOS o `chrono mcp --repos DIR`),
// `<nombre>@<rama>` o una ruta absoluta. Sin `repo` responde el índice del
// cwd, como siempre. Toda respuesta lleva "repo": "<nombre>@<rama>".
package mcp

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"

	"github.com/juan52878911/chrono/internal/config"
	"github.com/juan52878911/chrono/internal/i18n"
	"github.com/juan52878911/chrono/internal/metrics"
	"github.com/juan52878911/chrono/internal/repos"
	"github.com/juan52878911/chrono/internal/store"
)

type request struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params"`
}

// Version es la que se anuncia en initialize (la pone main desde -ldflags).
var Version = "dev"

type toolDef struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	InputSchema map[string]any `json:"inputSchema"`
}

// Server es el estado de una sesión MCP: el índice por defecto (el del cwd,
// puede no haber) y los índices de otros repos, abiertos bajo demanda.
type Server struct {
	def      *store.Store
	defLabel string
	defDir   string
	root     string
	open     map[string]*store.Store // índice → store abierto
}

// New prepara el servidor. def puede ser nil (sin índice en el cwd); root
// puede ser "" (sin raíz de repos: `repo` solo acepta rutas absolutas).
func New(def *store.Store, root string) *Server {
	s := &Server{def: def, root: root, open: map[string]*store.Store{}}
	if def != nil {
		if rp, ok, _ := def.Meta("repo_path"); ok && rp != "" {
			s.defDir = rp
			s.defLabel = repos.Label(rp)
		}
	}
	return s
}

// Close cierra los índices abiertos bajo demanda (el por defecto es del llamador).
func (s *Server) Close() {
	for _, st := range s.open {
		st.Close()
	}
}

// Serve corre el bucle de lectura/respuesta sobre stdin/stdout.
func Serve(st *store.Store, root string) error {
	s := New(st, root)
	defer s.Close()
	return s.Run(os.Stdin, os.Stdout)
}

// Run atiende JSON-RPC línea a línea de r hasta EOF.
func (s *Server) Run(r io.Reader, w io.Writer) error {
	in := bufio.NewScanner(r)
	in.Buffer(make([]byte, 1024*1024), 16*1024*1024)
	out := bufio.NewWriter(w)
	defer out.Flush()
	for in.Scan() {
		line := in.Bytes()
		if len(line) == 0 {
			continue
		}
		if resp := s.Handle(line); resp != nil {
			out.Write(resp)
			out.WriteByte('\n')
			out.Flush()
		}
	}
	return in.Err()
}

// Handle despacha un mensaje JSON-RPC y devuelve la respuesta (nil si no toca).
func (s *Server) Handle(line []byte) []byte {
	var req request
	if err := json.Unmarshal(line, &req); err != nil {
		return nil
	}
	send := func(result any, errObj any) []byte {
		resp := map[string]any{"jsonrpc": "2.0", "id": json.RawMessage(req.ID)}
		if len(req.ID) == 0 {
			resp["id"] = nil
		}
		if errObj != nil {
			resp["error"] = errObj
		} else {
			resp["result"] = result
		}
		b, _ := json.Marshal(resp)
		return b
	}
	switch req.Method {
	case "initialize":
		return send(map[string]any{
			"protocolVersion": "2024-11-05",
			"capabilities":    map[string]any{"tools": map[string]any{}},
			"serverInfo":      map[string]any{"name": "chrono", "version": Version},
		}, nil)
	case "notifications/initialized":
		return nil
	case "ping":
		return send(map[string]any{}, nil)
	case "tools/list":
		return send(map[string]any{"tools": tools()}, nil)
	case "tools/call":
		result, err := s.call(req.Params)
		if err != nil {
			return send(nil, map[string]any{"code": -32000, "message": err.Error()})
		}
		return send(result, nil)
	default:
		if len(req.ID) > 0 {
			return send(nil, map[string]any{"code": -32601, "message": "método no soportado: " + req.Method})
		}
	}
	return nil
}

func strProp(desc string) map[string]any {
	return map[string]any{"type": "string", "description": desc}
}

const repoDesc = "Repo to answer about (optional): a name under the repos root (CHRONO_REPOS / --repos), <name>@<branch>, or an absolute path. Default: the repo of the server's working directory."

func tools() []toolDef {
	since := strProp("Ventana temporal ISO, p.ej. 2025-01-01 (opcional)")
	obj := func(props map[string]any, required ...string) map[string]any {
		props["repo"] = strProp(repoDesc)
		m := map[string]any{"type": "object", "properties": props}
		if len(required) > 0 {
			m["required"] = required
		}
		return m
	}
	return []toolDef{
		{"hotspots", "Ficheros que más cambian y más pesan.", obj(map[string]any{"since": since})},
		{"coupling", "Qué ficheros cambian junto a uno dado.", obj(map[string]any{"file": strProp("Ruta del fichero relativa a la raíz del repo; acepta ruta#símbolo (se ignora el #símbolo)"), "since": since}, "file")},
		{"owners", "Propiedad por autor y bus factor de una ruta.", obj(map[string]any{"path": strProp("Prefijo de ruta relativo a la raíz del repo; acepta ruta#símbolo"), "since": since}, "path")},
		{"bugs", "Zonas donde se concentran los fixes (relacional).", obj(map[string]any{"since": since})},
		{"churn", "Líneas añadidas/borradas por fichero.", obj(map[string]any{"since": since})},
		{"tickets", "Commits y ficheros ligados a un ticket.", obj(map[string]any{"id": strProp("Id del ticket, p.ej. PROJ-123 o #45")}, "id")},
		{"phases", "Fases del proyecto (etiquetas/releases).", obj(map[string]any{})},
		{"prs", "Pull requests del forge (estado, merge, si es bug).", obj(map[string]any{"since": since})},
		{"branches", "Estado de ramas vs base: ahead/behind, mergeada, stale, autores.", obj(map[string]any{"base": strProp("Rama base (opcional; por defecto main/master)")})},
		{"search", "Busca commits por significado (texto completo).", obj(map[string]any{"query": strProp("Texto a buscar")}, "query")},
		{"similar", "Commits casi-duplicados de un SHA (por SimHash).", obj(map[string]any{"sha": strProp("SHA del commit")}, "sha")},
		{"repos", "Repos under the repos root and whether each has a chrono index (label <name>@<branch>).", map[string]any{"type": "object", "properties": map[string]any{}}},
	}
}

func textResult(text string, isErr bool) map[string]any {
	r := map[string]any{"content": []map[string]any{{"type": "text", "text": text}}}
	if isErr {
		r["isError"] = true
	}
	return r
}

// target elige el índice de la llamada. Devuelve (store, dir, etiqueta) o un
// resultado de tool con el error ya redactado para el agente.
func (s *Server) target(spec string) (*store.Store, string, string, map[string]any) {
	if spec == "" {
		if s.def == nil {
			return nil, "", "", textResult(i18n.T(
				"chrono: this repository has no index yet. Run 'chrono init' at its root and retry.",
				"chrono: este repositorio aún no tiene índice. Ejecuta 'chrono init' en su raíz y reintenta."), false)
		}
		return s.def, s.defDir, s.defLabel, nil
	}
	// Sin raíz, el nombre (o etiqueta) del repo por defecto sigue valiendo.
	if s.root == "" && s.def != nil && s.defLabel != "" {
		n, _ := repos.SplitLabel(s.defLabel)
		if spec == s.defLabel || spec == n {
			return s.def, s.defDir, s.defLabel, nil
		}
	}
	t, err := repos.Resolve(s.root, spec)
	if err != nil {
		return nil, "", "", textResult("chrono: "+err.Error(), true)
	}
	if st, ok := s.open[t.Index]; ok {
		return st, t.Dir, t.Label, nil
	}
	st, err := store.Open(t.Index)
	if err == nil {
		err = st.Migrate()
	}
	if err != nil {
		return nil, "", "", textResult(fmt.Sprintf("chrono: cannot open the index of %s: %v", t.Label, err), true)
	}
	s.open[t.Index] = st
	return st, t.Dir, t.Label, nil
}

func (s *Server) call(params json.RawMessage) (map[string]any, error) {
	var p struct {
		Name      string `json:"name"`
		Arguments struct {
			Repo  string `json:"repo"`
			File  string `json:"file"`
			Path  string `json:"path"`
			ID    string `json:"id"`
			Since string `json:"since"`
			Query string `json:"query"`
			SHA   string `json:"sha"`
			Base  string `json:"base"`
		} `json:"arguments"`
	}
	if err := json.Unmarshal(params, &p); err != nil {
		return nil, err
	}
	a := p.Arguments
	if p.Name == "repos" {
		return s.listRepos()
	}
	st, dir, label, errRes := s.target(a.Repo)
	if errRes != nil {
		return errRes, nil
	}
	db := st.DB()
	var result map[string]any
	var err error
	switch p.Name {
	case "hotspots":
		var r any
		r, err = metrics.Hotspots(db, a.Since, 25)
		result = map[string]any{"hotspots": r}
	case "coupling":
		file := repos.EntityKey(a.File, dir)
		if file == "" {
			return nil, errors.New("coupling: missing required argument 'file'")
		}
		var pairs any
		pairs, _, err = metrics.Coupling(db, file, 2, a.Since, 25)
		result = map[string]any{"for": file, "coupled": pairs}
	case "owners":
		path := repos.EntityKey(a.Path, dir)
		var owners any
		var bf int
		owners, bf, err = metrics.Owners(db, path, a.Since)
		result = map[string]any{"path": path, "owners": owners, "bus_factor": bf}
	case "bugs":
		rp, _, _ := st.Meta("repo_path")
		result, err = metrics.CommonBugs(db, config.Load(rp).BugCategories, a.Since, 15)
	case "churn":
		var r any
		r, err = metrics.Churn(db, a.Since, 25)
		result = map[string]any{"churn": r}
	case "tickets":
		result, err = metrics.Ticket(db, a.ID)
	case "phases":
		var r any
		r, err = metrics.Phases(db)
		result = map[string]any{"phases": r}
	case "prs":
		var r any
		r, err = metrics.PullRequests(db, a.Since, 30)
		result = map[string]any{"pull_requests": r}
	case "branches":
		rp, ok, _ := st.Meta("repo_path")
		if !ok || rp == "" {
			err = fmt.Errorf("no repo_path in the index")
			break
		}
		var base, cur string
		var brs any
		base, cur, brs, err = metrics.Branches(rp, a.Base)
		result = map[string]any{"base": base, "current": cur, "branches": brs}
	case "search":
		var r any
		r, err = metrics.Search(db, st.HasFTS(), a.Query, 25)
		result = map[string]any{"query": a.Query, "results": r}
	case "similar":
		var r any
		r, err = metrics.Similar(db, a.SHA, 4, 15)
		result = map[string]any{"of": a.SHA, "similar": r}
	default:
		return nil, fmt.Errorf("herramienta desconocida: %s", p.Name)
	}
	if err != nil {
		return nil, err
	}
	if result == nil {
		result = map[string]any{}
	}
	if label != "" {
		result["repo"] = label
	}
	text, _ := json.MarshalIndent(result, "", "  ")
	return textResult(string(text), false), nil
}

func (s *Server) listRepos() (map[string]any, error) {
	out := map[string]any{"root": s.root}
	if s.defLabel != "" {
		out["default"] = s.defLabel
	}
	if s.root == "" {
		out["repos"] = []repos.Entry{}
		out["note"] = "no repos root configured: set " + repos.EnvRoot + " or run `chrono mcp --repos DIR`"
	} else {
		list, err := repos.List(s.root)
		if err != nil {
			return textResult("chrono: cannot list "+s.root+": "+err.Error(), true), nil
		}
		if list == nil {
			list = []repos.Entry{}
		}
		out["repos"] = list
	}
	text, _ := json.MarshalIndent(out, "", "  ")
	return textResult(string(text), false), nil
}
