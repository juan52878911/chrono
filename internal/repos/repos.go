// Package repos resuelve QUÉ repo responde una consulta cuando un solo
// proceso (p.ej. `chrono mcp`) sirve muchos: un nombre bajo una raíz de repos,
// la etiqueta `<nombre>@<rama>` o una ruta absoluta. Es el contrato de
// docs/CONTRACTS.md: la etiqueta de repo y la clave de entidad (`ruta` o
// `ruta#símbolo`) que chrono comparte con UltraMemory y codegraph.
//
// No hace falta git para resolver: la rama se lee de .git/HEAD y los
// worktrees de .git/worktrees/*, así la resolución es determinista y barata.
package repos

import (
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

// IndexRel es dónde vive el índice de un repo, relativo a su raíz. Con muchos
// repos bajo una raíz, cada uno lleva el suyo: <raíz>/<nombre>/.chrono/index.db.
const IndexRel = ".chrono/index.db"

// EnvRoot es la variable de entorno con la raíz de repos (p.ej. /data/repos).
const EnvRoot = "CHRONO_REPOS"

// ErrNoIndex marca un repo que existe pero aún no tiene índice.
var ErrNoIndex = errors.New("no chrono index")

// Target es un repo ya resuelto.
type Target struct {
	Dir   string // raíz del árbol de trabajo (absoluta)
	Index string // <Dir>/.chrono/index.db
	Label string // <nombre>@<rama>
}

// IndexPath devuelve dónde está (o estaría) el índice de dir.
func IndexPath(dir string) string { return filepath.Join(dir, filepath.FromSlash(IndexRel)) }

// HasIndex dice si dir tiene índice.
func HasIndex(dir string) bool {
	fi, err := os.Stat(IndexPath(dir))
	return err == nil && !fi.IsDir()
}

// gitDir devuelve el directorio git de un árbol: .git si es carpeta, o el
// destino de "gitdir: X" si .git es un fichero (worktree o submódulo).
func gitDir(dir string) (string, bool) {
	p := filepath.Join(dir, ".git")
	fi, err := os.Stat(p)
	if err != nil {
		return "", false
	}
	if fi.IsDir() {
		return p, true
	}
	b, err := os.ReadFile(p)
	if err != nil {
		return "", false
	}
	s := strings.TrimSpace(string(b))
	if !strings.HasPrefix(s, "gitdir:") {
		return "", false
	}
	g := strings.TrimSpace(strings.TrimPrefix(s, "gitdir:"))
	if !filepath.IsAbs(g) {
		g = filepath.Join(dir, g)
	}
	return g, true
}

// headBranch lee la rama de un fichero HEAD: "ref: refs/heads/X" → X. Con
// HEAD separado devuelve "HEAD".
func headBranch(headFile string) string {
	b, err := os.ReadFile(headFile)
	if err != nil {
		return ""
	}
	s := strings.TrimSpace(string(b))
	if r, ok := strings.CutPrefix(s, "ref:"); ok {
		r = strings.TrimSpace(r)
		return strings.TrimPrefix(r, "refs/heads/")
	}
	return "HEAD"
}

// Branch es la rama actual del árbol dir ("" si no es un repo git).
func Branch(dir string) string {
	g, ok := gitDir(dir)
	if !ok {
		return ""
	}
	return headBranch(filepath.Join(g, "HEAD"))
}

// Label es la etiqueta `<nombre>@<rama>` de un árbol. El nombre es el del
// repo principal (un worktree de kindling se etiqueta kindling@<rama>, no con
// el nombre de su carpeta). Sin git, solo `<nombre>`.
func Label(dir string) string {
	name := filepath.Base(dir)
	g, ok := gitDir(dir)
	if !ok {
		return name
	}
	// Worktree: .git/worktrees/<id> → el repo principal es el padre de .git.
	if parent := filepath.Dir(g); filepath.Base(parent) == "worktrees" {
		name = filepath.Base(filepath.Dir(filepath.Dir(parent)))
	}
	br := headBranch(filepath.Join(g, "HEAD"))
	if br == "" {
		return name
	}
	return name + "@" + br
}

// SplitLabel separa `<nombre>@<rama>`. La rama puede llevar '/', el nombre no.
func SplitLabel(s string) (name, branch string) {
	if i := strings.IndexByte(s, '@'); i >= 0 {
		return s[:i], s[i+1:]
	}
	return s, ""
}

// worktrees lista los worktrees enlazados de un repo: rama → ruta.
func worktrees(dir string) map[string]string {
	out := map[string]string{}
	g, ok := gitDir(dir)
	if !ok {
		return out
	}
	ents, err := os.ReadDir(filepath.Join(g, "worktrees"))
	if err != nil {
		return out
	}
	for _, e := range ents {
		wd := filepath.Join(g, "worktrees", e.Name())
		br := headBranch(filepath.Join(wd, "HEAD"))
		b, err := os.ReadFile(filepath.Join(wd, "gitdir"))
		if br == "" || br == "HEAD" || err != nil {
			continue
		}
		// gitdir apunta al fichero .git del worktree.
		p := strings.TrimSpace(string(b))
		if !filepath.IsAbs(p) {
			p = filepath.Join(wd, p)
		}
		out[br] = filepath.Dir(filepath.Clean(p))
	}
	return out
}

// Slug es el nombre de directorio de una rama (mismo criterio que
// UltraMemory `ramas.py`: lo que no sea [A-Za-z0-9._-] se vuelve '-').
func Slug(branch string) string {
	var b strings.Builder
	dash := false
	for _, r := range branch {
		ok := r < 128 && (r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z' || r >= '0' && r <= '9' || r == '.' || r == '_' || r == '-')
		if ok {
			b.WriteRune(r)
			dash = false
		} else if !dash {
			b.WriteByte('-')
			dash = true
		}
	}
	s := strings.Trim(b.String(), "-.")
	if len(s) > 80 {
		s = s[:80]
	}
	if s == "" {
		return "rama"
	}
	return s
}

func isDir(p string) bool {
	fi, err := os.Stat(p)
	return err == nil && fi.IsDir()
}

// validName rechaza nombres que escaparían de la raíz.
func validName(n string) bool {
	return n != "" && n != "." && n != ".." && !strings.ContainsAny(n, `/\`) && !strings.HasPrefix(n, ".")
}

// Resolve convierte el argumento `repo` en un árbol con su índice:
//
//   - ruta absoluta → ese árbol;
//   - `<nombre>` → <root>/<nombre>;
//   - `<nombre>@<rama>` → primero el directorio (o symlink) literal
//     <root>/<nombre>@<rama con '/'→'-'> (o su Slug); si no existe,
//     <root>/<nombre> si está en esa rama; si no, el worktree de esa rama
//     (git worktree), o <root>/../ramas/<nombre>/<slug> (UltraMemory).
//
// Si el árbol existe pero no tiene índice, devuelve el Target y un error que
// envuelve ErrNoIndex con la orden exacta de `chrono init`.
func Resolve(root, spec string) (Target, error) {
	spec = strings.TrimSpace(spec)
	if spec == "" {
		return Target{}, errors.New("empty repo")
	}
	var dir string
	if filepath.IsAbs(spec) {
		dir = filepath.Clean(spec)
		if !isDir(dir) {
			return Target{}, fmt.Errorf("repo %q: no such directory", spec)
		}
	} else {
		name, branch := SplitLabel(spec)
		if !validName(name) {
			return Target{}, fmt.Errorf("repo %q: expected a name, <name>@<branch> or an absolute path", spec)
		}
		if root == "" {
			return Target{}, fmt.Errorf("repo %q: no repos root configured (set %s or run `chrono mcp --repos DIR`), or pass an absolute path", spec, EnvRoot)
		}
		base := filepath.Join(root, name)
		if branch != "" {
			// Un directorio o symlink con la etiqueta como nombre manda: es
			// lo que monta el operador (p.ej. UltraMemory: kindling@feature-x).
			for _, c := range []string{name + "@" + strings.ReplaceAll(branch, "/", "-"), name + "@" + Slug(branch)} {
				if p := filepath.Join(root, c); isDir(p) {
					dir = p
					break
				}
			}
		}
		if dir == "" {
			if !isDir(base) {
				return Target{}, fmt.Errorf("repo %q: %s does not exist (the `repos` tool lists what is under %s)", spec, base, root)
			}
			dir = base
		}
		if dir == base && branch != "" && Branch(base) != branch {
			dir = ""
			if wt, ok := worktrees(base)[branch]; ok && isDir(wt) {
				dir = wt
			}
			if c := filepath.Join(filepath.Dir(root), "ramas", name, Slug(branch)); dir == "" && isDir(c) && Branch(c) == branch {
				dir = c
			}
			if dir == "" {
				return Target{}, fmt.Errorf("repo %q: branch %q is not checked out (neither %s nor any of its worktrees is on it)", spec, branch, base)
			}
		}
	}
	if abs, err := filepath.Abs(dir); err == nil {
		dir = abs
	}
	t := Target{Dir: dir, Index: IndexPath(dir), Label: Label(dir)}
	if !HasIndex(dir) {
		return t, fmt.Errorf("%w for %s (%s). Run: chrono init %s", ErrNoIndex, t.Label, dir, dir)
	}
	return t, nil
}

// Entry es una fila de List.
type Entry struct {
	Repo    string `json:"repo"`    // <nombre>@<rama>
	Name    string `json:"name"`    // directorio bajo la raíz
	Path    string `json:"path"`    // ruta absoluta
	Indexed bool   `json:"indexed"` // tiene .chrono/index.db
}

// List recorre los directorios de primer nivel de root que son repos git (o
// tienen índice) y dice cuáles están indexados. Orden por nombre.
func List(root string) ([]Entry, error) {
	ents, err := os.ReadDir(root)
	if err != nil {
		return nil, err
	}
	var out []Entry
	for _, e := range ents {
		dir := filepath.Join(root, e.Name())
		// isDir sigue symlinks (y descarta los colgantes).
		if strings.HasPrefix(e.Name(), ".") || !isDir(dir) {
			continue
		}
		_, git := gitDir(dir)
		idx := HasIndex(dir)
		if !git && !idx {
			continue
		}
		out = append(out, Entry{Repo: Label(dir), Name: e.Name(), Path: dir, Indexed: idx})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out, nil
}

// EntityKey normaliza una clave de entidad de entrada a la ruta de fichero
// relativa a la raíz del repo: quita `#símbolo` (una respuesta a nivel de
// fichero lo ignora, así un id de codegraph `ruta#símbolo` entra tal cual),
// quita "./" y hace relativa una ruta absoluta que cae dentro de repoDir.
func EntityKey(key, repoDir string) string {
	k := strings.TrimSpace(key)
	if i := strings.IndexByte(k, '#'); i >= 0 {
		k = k[:i]
	}
	if repoDir != "" && filepath.IsAbs(k) {
		if rel, err := filepath.Rel(repoDir, k); err == nil && !strings.HasPrefix(rel, "..") {
			k = rel
		}
	}
	k = filepath.ToSlash(k)
	for strings.HasPrefix(k, "./") {
		k = k[2:]
	}
	return k
}
