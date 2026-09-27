package dataset

import (
	"bufio"
	"bytes"
	"encoding/json"
	"path/filepath"
	"testing"

	"github.com/juan52878911/chrono/internal/domain"
	"github.com/juan52878911/chrono/internal/store"
)

func TestExportFormatoChispa(t *testing.T) {
	st, err := store.Open(filepath.Join(t.TempDir(), "index.db"))
	if err != nil {
		t.Fatal(err)
	}
	defer st.Close()
	if err := st.Migrate(); err != nil {
		t.Fatal(err)
	}
	w, _ := st.NewWriter()
	add := func(sha, when, subj, body, kind, src string, paths ...string) {
		var ch []domain.FileChange
		for _, p := range paths {
			ch = append(ch, domain.FileChange{Path: p, Type: domain.Modified, Added: 5, Deleted: 2})
		}
		rev := domain.Revision{SHA: sha, AuthorName: "A", AuthorEmail: "a@x", When: when, Subject: subj, Body: body, Changes: ch}
		if err := w.AddCommit(rev, domain.Classification{Kind: kind, IsFix: kind == "fix", Source: src}, nil); err != nil {
			t.Fatal(err)
		}
	}
	add("c2", "2025-02-01T00:00:00Z", "feat(api): add endpoint", "Body line\nSigned-off-by: A <a@x>\n- fix: leaked bullet", "feat", "rules", "api/h.go", "api/h_test.go", "README.md")
	add("c1", "2025-01-01T00:00:00Z", "fix: crash on empty cache", "", "fix", "forge", "cache/c.go")
	add("c3", "2025-03-01T00:00:00Z", "tidy things up", "", "other", "rules", "x.go")
	add("c4", "2025-04-01T00:00:00Z", "fix: Crash on  empty cache", "", "fix", "rules", "cache/c.go") // duplicado
	if err := w.Commit(); err != nil {
		t.Fatal(err)
	}

	var buf bytes.Buffer
	n, err := Export(st.DB(), &buf, Options{Repo: "demo@main"})
	if err != nil {
		t.Fatal(err)
	}
	if n != 2 {
		t.Fatalf("filas = %d, want 2 (sin 'other' ni duplicados):\n%s", n, buf.String())
	}
	var rows []map[string]any
	sc := bufio.NewScanner(&buf)
	for sc.Scan() {
		var r map[string]any
		if err := json.Unmarshal(sc.Bytes(), &r); err != nil {
			t.Fatal(err)
		}
		rows = append(rows, r)
	}
	// Orden temporal: c1 (enero) antes que c2 (febrero).
	if rows[0]["id"] != "c1" || rows[0]["label"] != "fix" || rows[0]["label_source"] != "forge" || rows[0]["text"] != "crash on empty cache" {
		t.Fatalf("fila 0: %v", rows[0])
	}
	r := rows[1]
	if r["text"] != "add endpoint\nBody line" || r["label"] != "feat" || r["repo"] != "demo@main" || r["time"].(float64) != 1738368000 {
		t.Fatalf("fila 1: %v", r)
	}
	f := r["fields"].(map[string]any)
	if f["files"].(float64) != 3 || f["churn"].(float64) != 21 {
		t.Fatalf("fields: %v", f)
	}
	if got, _ := json.Marshal(f["ext"]); string(got) != `[".go",".md"]` {
		t.Fatalf("ext: %s", got)
	}
	if got, _ := json.Marshal(f["dir"]); string(got) != `["api","(root)"]` {
		t.Fatalf("dir: %s", got)
	}
}

func TestTextRecortaSinPartirRunas(t *testing.T) {
	got := Text("docs: ñandú", "ááá", 3) // 3 bytes = á + medio á
	if got != "ñandú\ná" {
		t.Fatalf("%q", got)
	}
	if Text("fix: ", "cuerpo", 300) != "" {
		t.Fatal("un asunto vacío tras quitar el prefijo no se exporta")
	}
}
