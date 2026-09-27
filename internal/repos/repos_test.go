package repos

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// fakeRepo crea un árbol con .git/HEAD en la rama dada (sin ejecutar git).
func fakeRepo(t *testing.T, dir, branch string, indexed bool) {
	t.Helper()
	if err := os.MkdirAll(filepath.Join(dir, ".git"), 0o755); err != nil {
		t.Fatal(err)
	}
	write(t, filepath.Join(dir, ".git", "HEAD"), "ref: refs/heads/"+branch+"\n")
	if indexed {
		write(t, IndexPath(dir), "")
	}
}

// fakeWorktree enlaza un worktree de repo en la rama dada, como `git worktree add`.
func fakeWorktree(t *testing.T, repo, wt, id, branch string, indexed ...bool) {
	t.Helper()
	meta := filepath.Join(repo, ".git", "worktrees", id)
	if err := os.MkdirAll(meta, 0o755); err != nil {
		t.Fatal(err)
	}
	write(t, filepath.Join(meta, "HEAD"), "ref: refs/heads/"+branch+"\n")
	write(t, filepath.Join(meta, "gitdir"), filepath.Join(wt, ".git")+"\n")
	if err := os.MkdirAll(wt, 0o755); err != nil {
		t.Fatal(err)
	}
	write(t, filepath.Join(wt, ".git"), "gitdir: "+meta+"\n")
	if len(indexed) == 0 || indexed[0] {
		write(t, IndexPath(wt), "")
	}
}

func write(t *testing.T, p, s string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(p), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(p, []byte(s), 0o644); err != nil {
		t.Fatal(err)
	}
}

func TestResolveNombreEtiquetaYRuta(t *testing.T) {
	root := t.TempDir()
	fakeRepo(t, filepath.Join(root, "alpha"), "main", true)
	fakeRepo(t, filepath.Join(root, "beta"), "develop", false)
	fakeWorktree(t, filepath.Join(root, "alpha"), filepath.Join(t.TempDir(), "wt"), "wt1", "claude/feature-x", true)

	// Nombre.
	tg, err := Resolve(root, "alpha")
	if err != nil || tg.Label != "alpha@main" || tg.Dir != filepath.Join(root, "alpha") {
		t.Fatalf("alpha: %+v %v", tg, err)
	}
	// Nombre@rama en la rama del espejo.
	if tg, err = Resolve(root, "alpha@main"); err != nil || tg.Label != "alpha@main" {
		t.Fatalf("alpha@main: %+v %v", tg, err)
	}
	// Nombre@rama que vive en un worktree (la rama lleva '/').
	tg, err = Resolve(root, "alpha@claude/feature-x")
	if err != nil || tg.Label != "alpha@claude/feature-x" || !strings.HasSuffix(tg.Dir, "wt") {
		t.Fatalf("worktree: %+v %v", tg, err)
	}
	// Ruta absoluta (sin raíz configurada).
	if tg, err = Resolve("", filepath.Join(root, "alpha")); err != nil || tg.Label != "alpha@main" {
		t.Fatalf("abs: %+v %v", tg, err)
	}
}

func TestResolveDisposicionUltraMemory(t *testing.T) {
	data := t.TempDir()
	root := filepath.Join(data, "repos")
	fakeRepo(t, filepath.Join(root, "kindling"), "main", true)
	// /data/ramas/<repo>/<slug>, sin enlace de worktree en el espejo.
	fakeRepo(t, filepath.Join(data, "ramas", "kindling", "claude-fix-x"), "claude/fix-x", true)
	tg, err := Resolve(root, "kindling@claude/fix-x")
	if err != nil || tg.Dir != filepath.Join(data, "ramas", "kindling", "claude-fix-x") {
		t.Fatalf("ramas/: %+v %v", tg, err)
	}
}

func TestResolveErrores(t *testing.T) {
	root := t.TempDir()
	fakeRepo(t, filepath.Join(root, "beta"), "develop", false)

	_, err := Resolve(root, "beta")
	if !errors.Is(err, ErrNoIndex) || !strings.Contains(err.Error(), "chrono init "+filepath.Join(root, "beta")) {
		t.Fatalf("sin índice: %v", err)
	}
	if _, err := Resolve(root, "nope"); err == nil || !strings.Contains(err.Error(), "does not exist") {
		t.Fatalf("inexistente: %v", err)
	}
	if _, err := Resolve(root, "beta@main"); err == nil || !strings.Contains(err.Error(), "not checked out") {
		t.Fatalf("rama ausente: %v", err)
	}
	for _, bad := range []string{"../etc", "a/b", ".hidden", ".."} {
		if _, err := Resolve(root, bad); err == nil {
			t.Fatalf("%q debería rechazarse", bad)
		}
	}
	if _, err := Resolve("", "beta"); err == nil || !strings.Contains(err.Error(), EnvRoot) {
		t.Fatalf("sin raíz: %v", err)
	}
}

func TestList(t *testing.T) {
	root := t.TempDir()
	fakeRepo(t, filepath.Join(root, "b"), "main", false)
	fakeRepo(t, filepath.Join(root, "a"), "dev", true)
	os.MkdirAll(filepath.Join(root, "notarepo"), 0o755)
	got, err := List(root)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 2 || got[0].Repo != "a@dev" || !got[0].Indexed || got[1].Repo != "b@main" || got[1].Indexed {
		t.Fatalf("List: %+v", got)
	}
}

func TestEntityKey(t *testing.T) {
	cases := map[string]string{
		"src/a.go":             "src/a.go",
		"src/a.go#Parse":       "src/a.go",
		"./src/a.go#T.method":  "src/a.go",
		"/repo/src/a.go#x":     "src/a.go",
		"/elsewhere/a.go":      "/elsewhere/a.go",
		"  pkg/  ":             "pkg/",
		"internal/mcp/mcp.go#": "internal/mcp/mcp.go",
	}
	for in, want := range cases {
		if got := EntityKey(in, "/repo"); got != want {
			t.Errorf("EntityKey(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestSlugComoUltraMemory(t *testing.T) {
	for in, want := range map[string]string{
		"claude/kindling-docs-en-es-612886": "claude-kindling-docs-en-es-612886",
		"main":                              "main",
		"feat//x!":                          "feat-x",
	} {
		if got := Slug(in); got != want {
			t.Errorf("Slug(%q) = %q, want %q", in, got, want)
		}
	}
}

// Disposición de UltraMemory: `chrono mcp -repos /data/chrono/arboles`, una
// carpeta de symlinks `kindling` y `kindling@<slug>` a árboles con su índice.
func TestSymlinksComoUltraMemory(t *testing.T) {
	data := t.TempDir()
	main := filepath.Join(data, "repos", "kindling")
	wt := filepath.Join(data, "ramas", "kindling", "claude-feature-x")
	fakeRepo(t, main, "main", true)
	fakeWorktree(t, main, wt, "claude-feature-x", "claude/feature-x", true)
	root := filepath.Join(data, "arboles")
	os.MkdirAll(root, 0o755)
	for name, target := range map[string]string{
		"kindling":                  main,
		"kindling@claude-feature-x": wt,
		"kindling@feature-y":        wt, // el literal manda aunque la rama real sea otra
		"roto@x":                    filepath.Join(data, "no-existe"),
	} {
		if err := os.Symlink(target, filepath.Join(root, name)); err != nil {
			t.Fatal(err)
		}
	}
	for spec, wantDir := range map[string]string{
		"kindling":                  filepath.Join(root, "kindling"),
		"kindling@claude-feature-x": filepath.Join(root, "kindling@claude-feature-x"),
		"kindling@claude/feature-x": filepath.Join(root, "kindling@claude-feature-x"),
		"kindling@feature-y":        filepath.Join(root, "kindling@feature-y"),
	} {
		tg, err := Resolve(root, spec)
		if err != nil || tg.Dir != wantDir {
			t.Fatalf("%s: %+v %v", spec, tg, err)
		}
	}
	if tg, _ := Resolve(root, "kindling@claude-feature-x"); tg.Label != "kindling@claude/feature-x" {
		t.Fatalf("etiqueta del worktree: %s", tg.Label)
	}
	got, err := List(root)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, e := range got {
		names = append(names, e.Name+"="+e.Repo)
	}
	want := "kindling=kindling@main kindling@claude-feature-x=kindling@claude/feature-x kindling@feature-y=kindling@claude/feature-x"
	if strings.Join(names, " ") != want {
		t.Fatalf("List (sin el symlink roto):\n got %s\nwant %s", strings.Join(names, " "), want)
	}
}
