package mcp

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/juan52878911/chrono/internal/domain"
	"github.com/juan52878911/chrono/internal/store"
)

// repoWithIndex crea <dir> con .git/HEAD en `branch` y un índice real cuyos
// commits tocan los ficheros dados (dos commits: a+b juntos, dos veces).
func repoWithIndex(t *testing.T, dir, branch string, files ...string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Join(dir, ".git"), 0o755); err != nil {
		t.Fatal(err)
	}
	os.WriteFile(filepath.Join(dir, ".git", "HEAD"), []byte("ref: refs/heads/"+branch+"\n"), 0o644)
	os.MkdirAll(filepath.Join(dir, ".chrono"), 0o755)
	st, err := store.Open(filepath.Join(dir, ".chrono", "index.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	if err := st.Migrate(); err != nil {
		t.Fatal(err)
	}
	st.SetMeta("repo_path", dir)
	w, err := st.NewWriter()
	if err != nil {
		t.Fatal(err)
	}
	for i, sha := range []string{"a1", "a2"} {
		var ch []domain.FileChange
		for _, f := range files {
			ch = append(ch, domain.FileChange{Path: f, Type: domain.Modified, Added: 3, Deleted: 1})
		}
		rev := domain.Revision{SHA: sha, AuthorName: "Ana", AuthorEmail: "ana@x", When: "2025-01-0" + string(rune('1'+i)) + "T10:00:00Z",
			Subject: "fix: cosa " + sha, Changes: ch}
		if err := w.AddCommit(rev, domain.Classification{Kind: "fix", IsFix: true, Source: "rules"}, nil); err != nil {
			t.Fatal(err)
		}
	}
	if err := w.Commit(); err != nil {
		t.Fatal(err)
	}
}

func callTool(t *testing.T, s *Server, name string, args map[string]any) (map[string]any, bool) {
	t.Helper()
	req, _ := json.Marshal(map[string]any{"jsonrpc": "2.0", "id": 1, "method": "tools/call",
		"params": map[string]any{"name": name, "arguments": args}})
	var resp struct {
		Result struct {
			Content []struct{ Text string } `json:"content"`
			IsError bool                    `json:"isError"`
		} `json:"result"`
		Error *struct{ Message string } `json:"error"`
	}
	if err := json.Unmarshal(s.Handle(req), &resp); err != nil {
		t.Fatal(err)
	}
	if resp.Error != nil {
		t.Fatalf("%s: error JSON-RPC: %s", name, resp.Error.Message)
	}
	text := resp.Result.Content[0].Text
	if resp.Result.IsError {
		return map[string]any{"error": text}, true
	}
	var out map[string]any
	if err := json.Unmarshal([]byte(text), &out); err != nil {
		return map[string]any{"text": text}, false
	}
	return out, false
}

func firstHotspot(t *testing.T, out map[string]any) string {
	t.Helper()
	hs, _ := out["hotspots"].([]any)
	if len(hs) == 0 {
		t.Fatalf("sin hotspots: %v", out)
	}
	return hs[0].(map[string]any)["path"].(string)
}

func TestRepoArg(t *testing.T) {
	root := t.TempDir()
	repoWithIndex(t, filepath.Join(root, "alpha"), "main", "alpha.go", "shared.go")
	repoWithIndex(t, filepath.Join(root, "beta"), "dev", "beta.go", "other.go")
	os.MkdirAll(filepath.Join(root, "gamma", ".git"), 0o755)
	os.WriteFile(filepath.Join(root, "gamma", ".git", "HEAD"), []byte("ref: refs/heads/main\n"), 0o644)

	// El por defecto (cwd) es alpha.
	def, _ := store.Open(filepath.Join(root, "alpha", ".chrono", "index.db"))
	defer def.Close()
	def.Migrate()
	s := New(def, root)
	defer s.Close()

	// Sin repo: el de siempre, etiquetado.
	out, _ := callTool(t, s, "hotspots", nil)
	if out["repo"] != "alpha@main" {
		t.Fatalf("repo por defecto: %v", out["repo"])
	}
	// Por nombre: otro índice, otras respuestas.
	out, _ = callTool(t, s, "churn", map[string]any{"repo": "beta"})
	if out["repo"] != "beta@dev" || !strings.Contains(mustJSON(out), "beta.go") {
		t.Fatalf("beta: %v", out)
	}
	// nombre@rama.
	out, _ = callTool(t, s, "hotspots", map[string]any{"repo": "beta@dev"})
	if out["repo"] != "beta@dev" {
		t.Fatalf("beta@dev: %v", out)
	}
	if p := firstHotspot(t, out); p != "beta.go" && p != "other.go" {
		t.Fatalf("beta@dev hotspot: %s", p)
	}
	// Ruta absoluta.
	out, _ = callTool(t, s, "hotspots", map[string]any{"repo": filepath.Join(root, "beta")})
	if out["repo"] != "beta@dev" {
		t.Fatalf("abs: %v", out)
	}
	// Sin índice: error claro con la orden de init.
	out, isErr := callTool(t, s, "hotspots", map[string]any{"repo": "gamma"})
	if !isErr || !strings.Contains(out["error"].(string), "chrono init "+filepath.Join(root, "gamma")) {
		t.Fatalf("gamma sin índice: %v", out)
	}
	// Rama que no está en disco.
	if out, isErr = callTool(t, s, "hotspots", map[string]any{"repo": "beta@main"}); !isErr {
		t.Fatalf("beta@main debería fallar: %v", out)
	}
	// path#symbol: se ignora el símbolo en respuestas a nivel de fichero.
	out, _ = callTool(t, s, "coupling", map[string]any{"repo": "alpha", "file": "alpha.go#Parse"})
	if out["for"] != "alpha.go" || !strings.Contains(mustJSON(out), "shared.go") {
		t.Fatalf("coupling path#symbol: %v", out)
	}
	out, _ = callTool(t, s, "owners", map[string]any{"path": filepath.Join(root, "alpha", "alpha.go") + "#X"})
	if out["path"] != "alpha.go" || out["bus_factor"].(float64) != 1 {
		t.Fatalf("owners abs#symbol: %v", out)
	}
	// repos lista lo que hay bajo la raíz.
	out, _ = callTool(t, s, "repos", nil)
	got := mustJSON(out["repos"])
	for _, want := range []string{`"repo":"alpha@main"`, `"repo":"beta@dev"`, `"repo":"gamma@main"`, `"indexed":false`} {
		if !strings.Contains(got, want) {
			t.Fatalf("repos: falta %s en %s", want, got)
		}
	}
}

func TestSinIndiceNiRaiz(t *testing.T) {
	s := New(nil, "")
	// Sin índice por defecto se responde texto (no error) pidiendo init, como siempre.
	out, isErr := callTool(t, s, "hotspots", nil)
	if isErr || !strings.Contains(out["text"].(string), "chrono init") {
		t.Fatalf("sin índice: %v", out)
	}
	out, isErr = callTool(t, s, "hotspots", map[string]any{"repo": "x"})
	if !isErr || !strings.Contains(out["error"].(string), "CHRONO_REPOS") {
		t.Fatalf("sin raíz: %v", out)
	}
}

func TestToolsListLlevaRepo(t *testing.T) {
	for _, td := range tools() {
		props := td.InputSchema["properties"].(map[string]any)
		if td.Name == "repos" {
			continue
		}
		if _, ok := props["repo"]; !ok {
			t.Errorf("%s sin argumento repo", td.Name)
		}
	}
}

func mustJSON(v any) string {
	b, _ := json.Marshal(v)
	return string(b)
}
