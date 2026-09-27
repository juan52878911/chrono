// Package dataset exporta los commits etiquetados del índice como JSONL en el
// formato exacto que consume `kling ai chispa train` (kindling, pkg/chispa):
//
//	{"text": "...", "label": "fix", "fields": {"files": 3, "churn": 42, "ext": [".go"], "dir": ["internal"]}, ...}
//
// Chispa es el único clasificador aprendido de chrono (docs/CONTRACTS.md): chrono
// no entrena ni embebe modelos; exporta datos y, como mucho, consulta un
// `.chispa` ya entrenado. Las claves extra (`id`, `repo`, `time`,
// `label_source`) las ignora Chispa al leer, igual que las de
// tools/chispa-commits en kindling, y sirven para repartir y auditar.
//
// Etiqueta = la clasificación determinista del índice (reglas Nivel 0 o label
// de bug del forge); los commits sin señal (`other`) no se exportan. El prefijo
// conventional commit se QUITA del texto: si no, el modelo aprende a leer las
// cuatro primeras letras (misma regla que docs/CHISPA-EVAL.md de kindling).
package dataset

import (
	"database/sql"
	"encoding/json"
	"io"
	"path"
	"regexp"
	"sort"
	"strings"
	"time"
	"unicode/utf8"
)

// DefaultBodyBytes es cuánto cuerpo se conserva tras el asunto (como kindling).
const DefaultBodyBytes = 300

var (
	prefixRe  = regexp.MustCompile(`(?i)^\s*(feat|fix|docs|refactor|test|chore|perf|ci|build|style)(\([^)]*\))?!?:\s*`)
	bulletRe  = regexp.MustCompile(`(?i)^\s*[-*]?\s*(feat|fix|docs|refactor|test|chore|perf|ci|build|style)(\([^)]*\))?!?:`)
	trailerRe = regexp.MustCompile(`(?i)^\s*(signed-off-by|co-authored-by|reviewed-by|acked-by|tested-by|reported-by|cc|change-id|fixes|closes|refs?)\s*:`)
)

// Options ajusta la exportación.
type Options struct {
	Since     string // ventana ISO (authored_at >= Since)
	BodyBytes int    // bytes de cuerpo tras el asunto; 0 = DefaultBodyBytes, <0 = sin cuerpo
	Repo      string // etiqueta <nombre>@<rama> que se escribe en cada fila
}

// Row es una línea del JSONL. text/label/fields son lo que ve Chispa.
type Row struct {
	Text        string         `json:"text"`
	Label       string         `json:"label"`
	Fields      map[string]any `json:"fields,omitempty"`
	ID          string         `json:"id"`
	Repo        string         `json:"repo,omitempty"`
	Time        int64          `json:"time"`
	LabelSource string         `json:"label_source"`
}

// Export escribe una fila por commit etiquetado, del más antiguo al más
// reciente (así un reparto temporal sin fugas es head/tail). Los textos
// repetidos se quedan en su primera aparición. Devuelve cuántas filas escribió.
func Export(db *sql.DB, w io.Writer, opt Options) (int, error) {
	since := opt.Since
	if since == "" {
		since = "0000"
	}
	body := opt.BodyBytes
	if body == 0 {
		body = DefaultBodyBytes
	}

	touches, err := loadTouches(db, since)
	if err != nil {
		return 0, err
	}
	rows, err := db.Query(`SELECT c.sha, c.authored_at, c.subject, COALESCE(c.body,''), cl.kind, cl.source
		FROM commits c JOIN classifications cl ON cl.commit_sha = c.sha
		WHERE cl.kind <> '' AND cl.kind <> 'other' AND c.authored_at >= ?
		ORDER BY unixepoch(c.authored_at), c.sha`, since)
	if err != nil {
		return 0, err
	}
	defer rows.Close()

	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	seen := map[string]bool{}
	n := 0
	for rows.Next() {
		var sha, at, subject, bodyText, kind, source string
		if err := rows.Scan(&sha, &at, &subject, &bodyText, &kind, &source); err != nil {
			return n, err
		}
		text := Text(subject, bodyText, body)
		if text == "" {
			continue
		}
		key := strings.ToLower(strings.Join(strings.Fields(text), " "))
		if seen[key] {
			continue
		}
		seen[key] = true
		var ts int64
		if t, err := time.Parse(time.RFC3339, at); err == nil {
			ts = t.Unix()
		}
		if source == "" {
			source = "rules"
		}
		r := Row{Text: text, Label: kind, Fields: Fields(touches[sha]), ID: sha, Repo: opt.Repo, Time: ts, LabelSource: source}
		if err := enc.Encode(r); err != nil {
			return n, err
		}
		n++
	}
	return n, rows.Err()
}

// Touch es un fichero tocado por un commit.
type Touch struct {
	Path           string
	Added, Deleted int
}

func loadTouches(db *sql.DB, since string) (map[string][]Touch, error) {
	rows, err := db.Query(`SELECT ch.commit_sha, f.path, ch.added, ch.deleted
		FROM changes ch JOIN files f ON f.id = ch.file_id JOIN commits c ON c.sha = ch.commit_sha
		WHERE c.authored_at >= ?`, since)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	out := map[string][]Touch{}
	for rows.Next() {
		var sha string
		var t Touch
		if err := rows.Scan(&sha, &t.Path, &t.Added, &t.Deleted); err != nil {
			return nil, err
		}
		out[sha] = append(out[sha], t)
	}
	return out, rows.Err()
}

// Text es lo que ve el modelo: el asunto sin prefijo conventional commit y,
// en otra línea, hasta n bytes de cuerpo sin trailers ni líneas con prefijo
// (los squash merges las listan: fuga de la etiqueta). "" si no queda asunto.
func Text(subject, body string, n int) string {
	s := strings.TrimSpace(subject)
	if m := prefixRe.FindString(s); m != "" {
		s = strings.TrimSpace(s[len(m):])
	}
	if s == "" {
		return ""
	}
	if n > 0 {
		if b := cleanBody(body, n); b != "" {
			s += "\n" + b
		}
	}
	return s
}

func cleanBody(b string, n int) string {
	var keep []string
	for _, l := range strings.Split(b, "\n") {
		if trailerRe.MatchString(l) || bulletRe.MatchString(l) {
			continue
		}
		if l = strings.TrimSpace(l); l != "" {
			keep = append(keep, l)
		}
	}
	s := strings.Join(keep, "\n")
	if len(s) > n {
		// Corta en el último inicio de runa <= n: no parte una runa UTF-8
		// ni tira una que cabe entera.
		for n > 0 && !utf8.RuneStart(s[n]) {
			n--
		}
		s = s[:n]
	}
	return s
}

// Fields resume los ficheros tocados como los campos de Chispa: cuántos
// (`files`), cuántas líneas (`churn`, binarios cuentan 0) y las ocho
// extensiones (`ext`) y directorios de primer nivel (`dir`) más frecuentes.
// nil si el commit no toca ficheros.
func Fields(ts []Touch) map[string]any {
	if len(ts) == 0 {
		return nil
	}
	churn := 0
	exts, dirs := map[string]int{}, map[string]int{}
	for _, t := range ts {
		churn += max(t.Added, 0) + max(t.Deleted, 0)
		ext := strings.ToLower(path.Ext(t.Path))
		if ext == "" {
			ext = "(none)"
		}
		exts[ext]++
		dir := "(root)"
		if i := strings.IndexByte(t.Path, '/'); i > 0 {
			dir = t.Path[:i]
		}
		dirs[dir]++
	}
	return map[string]any{"files": len(ts), "churn": churn, "ext": top(exts, 8), "dir": top(dirs, 8)}
}

func top(m map[string]int, n int) []string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Slice(keys, func(i, j int) bool {
		if m[keys[i]] != m[keys[j]] {
			return m[keys[i]] > m[keys[j]]
		}
		return keys[i] < keys[j]
	})
	return keys[:min(n, len(keys))]
}
