// Package mcp expone las consultas de chrono como un servidor MCP por stdio
// (JSON-RPC 2.0 delimitado por saltos de línea). Lo lanza el cliente (Claude)
// y muere con la sesión: no es un daemon.
package mcp

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"

	"github.com/juan52878911/chrono/internal/config"
	"github.com/juan52878911/chrono/internal/i18n"
	"github.com/juan52878911/chrono/internal/metrics"
	"github.com/juan52878911/chrono/internal/store"
)

type request struct {
	JSONRPC string          `json:"jsonrpc"`
	ID      json.RawMessage `json:"id"`
	Method  string          `json:"method"`
	Params  json.RawMessage `json:"params"`
}

type toolDef struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	InputSchema map[string]any `json:"inputSchema"`
}

// Serve corre el bucle de lectura/respuesta sobre stdin/stdout.
func Serve(st *store.Store) error {
	in := bufio.NewScanner(os.Stdin)
	in.Buffer(make([]byte, 1024*1024), 16*1024*1024)
	out := bufio.NewWriter(os.Stdout)
	defer out.Flush()

	send := func(id json.RawMessage, result any, errObj any) {
		resp := map[string]any{"jsonrpc": "2.0", "id": json.RawMessage(id)}
		if errObj != nil {
			resp["error"] = errObj
		} else {
			resp["result"] = result
		}
		b, _ := json.Marshal(resp)
		out.Write(b)
		out.WriteByte('\n')
		out.Flush()
	}

	for in.Scan() {
		line := in.Bytes()
		if len(line) == 0 {
			continue
		}
		var req request
		if err := json.Unmarshal(line, &req); err != nil {
			continue
		}
		switch req.Method {
		case "initialize":
			send(req.ID, map[string]any{
				"protocolVersion": "2024-11-05",
				"capabilities":    map[string]any{"tools": map[string]any{}},
				"serverInfo":      map[string]any{"name": "chrono", "version": "0.1.0"},
			}, nil)
		case "notifications/initialized":
			// notificación: sin respuesta.
		case "ping":
			send(req.ID, map[string]any{}, nil)
		case "tools/list":
			send(req.ID, map[string]any{"tools": tools()}, nil)
		case "tools/call":
			result, err := call(st, req.Params)
			if err != nil {
				send(req.ID, nil, map[string]any{"code": -32000, "message": err.Error()})
				continue
			}
			send(req.ID, result, nil)
		default:
			if len(req.ID) > 0 {
				send(req.ID, nil, map[string]any{"code": -32601, "message": "método no soportado: " + req.Method})
			}
		}
	}
	return in.Err()
}

func strProp(desc string) map[string]any {
	return map[string]any{"type": "string", "description": desc}
}

func tools() []toolDef {
	since := strProp("Ventana temporal ISO, p.ej. 2025-01-01 (opcional)")
	return []toolDef{
		{"hotspots", "Ficheros que más cambian y más pesan.", map[string]any{"type": "object", "properties": map[string]any{"since": since}}},
		{"coupling", "Qué ficheros cambian junto a uno dado.", map[string]any{"type": "object", "properties": map[string]any{"file": strProp("Ruta del fichero"), "since": since}, "required": []string{"file"}}},
		{"owners", "Propiedad por autor y bus factor de una ruta.", map[string]any{"type": "object", "properties": map[string]any{"path": strProp("Prefijo de ruta"), "since": since}, "required": []string{"path"}}},
		{"bugs", "Zonas donde se concentran los fixes (relacional).", map[string]any{"type": "object", "properties": map[string]any{"since": since}}},
		{"churn", "Líneas añadidas/borradas por fichero.", map[string]any{"type": "object", "properties": map[string]any{"since": since}}},
		{"tickets", "Commits y ficheros ligados a un ticket.", map[string]any{"type": "object", "properties": map[string]any{"id": strProp("Id del ticket, p.ej. PROJ-123 o #45")}, "required": []string{"id"}}},
		{"phases", "Fases del proyecto (etiquetas/releases).", map[string]any{"type": "object", "properties": map[string]any{}}},
		{"prs", "Pull requests del forge (estado, merge, si es bug).", map[string]any{"type": "object", "properties": map[string]any{"since": since}}},
		{"branches", "Estado de ramas vs base: ahead/behind, mergeada, stale, autores.", map[string]any{"type": "object", "properties": map[string]any{"base": strProp("Rama base (opcional; por defecto main/master)")}}},
		{"search", "Busca commits por significado (texto completo).", map[string]any{"type": "object", "properties": map[string]any{"query": strProp("Texto a buscar")}, "required": []string{"query"}}},
		{"similar", "Commits casi-duplicados de un SHA (por SimHash).", map[string]any{"type": "object", "properties": map[string]any{"sha": strProp("SHA del commit")}, "required": []string{"sha"}}},
	}
}

func call(st *store.Store, params json.RawMessage) (map[string]any, error) {
	if st == nil {
		return map[string]any{"content": []map[string]any{{"type": "text",
			"text": i18n.T(
				"chrono: this repository has no index yet. Run 'chrono init' at its root and retry.",
				"chrono: este repositorio aún no tiene índice. Ejecuta 'chrono init' en su raíz y reintenta.")}}}, nil
	}
	db := st.DB()
	var p struct {
		Name      string `json:"name"`
		Arguments struct {
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
	var result any
	var err error
	switch p.Name {
	case "hotspots":
		result, err = metrics.Hotspots(db, a.Since, 25)
	case "coupling":
		var pairs any
		pairs, _, err = metrics.Coupling(db, a.File, 2, a.Since, 25)
		result = pairs
	case "owners":
		var owners any
		var bf int
		owners, bf, err = metrics.Owners(db, a.Path, a.Since)
		result = map[string]any{"owners": owners, "bus_factor": bf}
	case "bugs":
		rp, _, _ := st.Meta("repo_path")
		result, err = metrics.CommonBugs(db, config.Load(rp).BugCategories, a.Since, 15)
	case "churn":
		result, err = metrics.Churn(db, a.Since, 25)
	case "tickets":
		result, err = metrics.Ticket(db, a.ID)
	case "phases":
		result, err = metrics.Phases(db)
	case "prs":
		result, err = metrics.PullRequests(db, a.Since, 30)
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
		result, err = metrics.Search(db, st.HasFTS(), a.Query, 25)
	case "similar":
		result, err = metrics.Similar(db, a.SHA, 4, 15)
	default:
		return nil, fmt.Errorf("herramienta desconocida: %s", p.Name)
	}
	if err != nil {
		return nil, err
	}
	text, _ := json.MarshalIndent(result, "", "  ")
	return map[string]any{"content": []map[string]any{{"type": "text", "text": string(text)}}}, nil
}
