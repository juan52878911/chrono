// Package metrics calcula las respuestas deterministas sobre el índice.
// Todas aceptan una ventana temporal (Since); un hotspot sin ventana es
// historia muerta.
package metrics

import (
	"database/sql"
	"sort"
	"strings"
	"time"

	"github.com/juan52878911/chrono/internal/domain"
	"github.com/juan52878911/chrono/internal/simhash"
)

// sinceClause devuelve la condición y el argumento para la ventana.
func sinceArg(since string) string {
	if since == "" {
		return "0000" // menor que cualquier fecha ISO.
	}
	return since
}

// Hotspots: ficheros que más cambian ponderado por su tamaño actual.
func Hotspots(db *sql.DB, since string, limit int) ([]domain.Hotspot, error) {
	rows, err := db.Query(`
		SELECT f.path, COUNT(*) AS changes, f.size_lines
		FROM changes c
		JOIN commits cm ON cm.sha=c.commit_sha
		JOIN files f ON f.id=c.file_id
		WHERE f.deleted=0 AND f.excluded=0 AND cm.authored_at >= ?
		GROUP BY f.id
		ORDER BY (COUNT(*)*1.0*f.size_lines) DESC
		LIMIT ?`, sinceArg(since), limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []domain.Hotspot
	var maxRaw float64
	for rows.Next() {
		var h domain.Hotspot
		if err := rows.Scan(&h.Path, &h.Changes, &h.SizeLines); err != nil {
			return nil, err
		}
		raw := float64(h.Changes) * float64(h.SizeLines)
		if raw > maxRaw {
			maxRaw = raw
		}
		h.Score = raw
		out = append(out, h)
	}
	if maxRaw > 0 {
		for i := range out {
			out[i].Score = out[i].Score / maxRaw
		}
	}
	return out, rows.Err()
}

// Coupling: qué cambia junto a `path`. Excluye commits is_bulk; poda por minSupport.
func Coupling(db *sql.DB, path string, minSupport int, since string, limit int) ([]domain.CouplingPair, int, error) {
	var total int
	err := db.QueryRow(`
		SELECT COUNT(*) FROM changes c
		JOIN files f ON f.id=c.file_id
		JOIN commits cm ON cm.sha=c.commit_sha
		WHERE f.path=? AND cm.is_bulk=0 AND cm.authored_at >= ?`, path, sinceArg(since)).Scan(&total)
	if err != nil {
		return nil, 0, err
	}
	rows, err := db.Query(`
		SELECT f.path, COUNT(*) AS support
		FROM changes c
		JOIN files f ON f.id=c.file_id
		WHERE c.commit_sha IN (
			SELECT c2.commit_sha FROM changes c2
			JOIN files f2 ON f2.id=c2.file_id
			JOIN commits cm ON cm.sha=c2.commit_sha
			WHERE f2.path=? AND cm.is_bulk=0 AND cm.authored_at >= ?
		) AND f.path != ?
		GROUP BY f.id
		HAVING support >= ?
		ORDER BY support DESC
		LIMIT ?`, path, sinceArg(since), path, minSupport, limit)
	if err != nil {
		return nil, 0, err
	}
	defer rows.Close()
	var out []domain.CouplingPair
	for rows.Next() {
		var p domain.CouplingPair
		if err := rows.Scan(&p.B, &p.Support); err != nil {
			return nil, 0, err
		}
		if total > 0 {
			p.Confidence = float64(p.Support) / float64(total)
		}
		out = append(out, p)
	}
	return out, total, rows.Err()
}

// Owners: propiedad por autor de una ruta (prefijo) + bus factor.
func Owners(db *sql.DB, prefix, since string) ([]domain.Ownership, int, error) {
	rows, err := db.Query(`
		SELECT a.display_name, COUNT(*) AS changes
		FROM changes c
		JOIN commits cm ON cm.sha=c.commit_sha
		JOIN files f ON f.id=c.file_id
		JOIN authors a ON a.id=cm.author_id
		WHERE f.path LIKE ? AND cm.authored_at >= ?
		GROUP BY cm.author_id
		ORDER BY changes DESC`, prefix+"%", sinceArg(since))
	if err != nil {
		return nil, 0, err
	}
	defer rows.Close()
	var out []domain.Ownership
	total := 0
	for rows.Next() {
		var o domain.Ownership
		if err := rows.Scan(&o.Name, &o.Changes); err != nil {
			return nil, 0, err
		}
		total += o.Changes
		out = append(out, o)
	}
	if err := rows.Err(); err != nil {
		return nil, 0, err
	}
	busFactor := 0
	acc := 0
	for i := range out {
		if total > 0 {
			out[i].Share = float64(out[i].Changes) / float64(total)
		}
		if acc < (total+1)/2 { // autores mínimos que cubren >=50%.
			acc += out[i].Changes
			busFactor++
		}
	}
	return out, busFactor, nil
}

// ChurnRow es una fila de churn por fichero.
type ChurnRow struct {
	Path    string `json:"path"`
	Added   int    `json:"added"`
	Deleted int    `json:"deleted"`
}

// Churn: líneas +/- por fichero en la ventana (top).
func Churn(db *sql.DB, since string, limit int) ([]ChurnRow, error) {
	rows, err := db.Query(`
		SELECT f.path, SUM(MAX(c.added,0)) AS a, SUM(MAX(c.deleted,0)) AS d
		FROM changes c
		JOIN commits cm ON cm.sha=c.commit_sha
		JOIN files f ON f.id=c.file_id
		WHERE f.excluded=0 AND cm.authored_at >= ?
		GROUP BY f.id
		ORDER BY (a+d) DESC
		LIMIT ?`, sinceArg(since), limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []ChurnRow
	for rows.Next() {
		var r ChurnRow
		if err := rows.Scan(&r.Path, &r.Added, &r.Deleted); err != nil {
			return nil, err
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// BugArea es una zona con concentración de fixes (respuesta RELACIONAL).
type BugArea struct {
	Dir           string   `json:"dir"`
	Fixes         int      `json:"fixes"`
	RecentFixes   int      `json:"recent_fixes"`
	RecencyWeight float64  `json:"recency_weight"`
	Examples      []string `json:"examples"`
}

// BugCategory es un tema recurrente entre los fixes (término saliente).
type BugCategory struct {
	Category string   `json:"category"`
	Fixes    int      `json:"fixes"`
	TopFiles []string `json:"top_files"`
	Examples []string `json:"examples"`
}

// CommonBugs responde "errores más comunes": DÓNDE se concentran (hot_areas,
// relacional por directorio) y de QUÉ tipo son (categories, por TAXONOMÍA de
// palabras clave -> categoría semántica). Determinista, configurable, sin modelo.
func CommonBugs(db *sql.DB, taxonomy map[string][]string, since string, limit int) (map[string]any, error) {
	var maxDate string
	_ = db.QueryRow(`SELECT MAX(authored_at) FROM commits`).Scan(&maxDate)
	recentThreshold := "0000"
	if t, err := time.Parse(time.RFC3339, maxDate); err == nil {
		recentThreshold = t.AddDate(0, 0, -90).Format(time.RFC3339)
	}

	// 1) Meta de cada commit fix (para categorías por término del mensaje).
	crows, err := db.Query(`
		SELECT cm.sha, cm.subject, COALESCE(cm.body,'')
		FROM commits cm JOIN classifications cl ON cl.commit_sha=cm.sha
		WHERE cl.is_fix=1 AND cm.is_bulk=0 AND cm.authored_at >= ?`, sinceArg(since))
	if err != nil {
		return nil, err
	}
	fixes := map[string]bool{}
	catCommits := map[string]map[string]bool{} // categoría -> set de sha
	for crows.Next() {
		var sha, subject, body string
		if err := crows.Scan(&sha, &subject, &body); err != nil {
			crows.Close()
			return nil, err
		}
		fixes[sha] = true
		text := strings.ToLower(subject + "\n" + cleanBody(body))
		for cat, kws := range taxonomy {
			for _, kw := range kws {
				if strings.Contains(text, strings.ToLower(kw)) {
					if catCommits[cat] == nil {
						catCommits[cat] = map[string]bool{}
					}
					catCommits[cat][sha] = true
					break
				}
			}
		}
	}
	crows.Close()

	// 2) Ficheros por commit fix (para hot_areas y ficheros por categoría).
	frows, err := db.Query(`
		SELECT c.commit_sha, f.path, cm.authored_at
		FROM changes c
		JOIN commits cm ON cm.sha=c.commit_sha
		JOIN classifications cl ON cl.commit_sha=cm.sha
		JOIN files f ON f.id=c.file_id
		WHERE cl.is_fix=1 AND cm.is_bulk=0 AND f.excluded=0 AND cm.authored_at >= ?`, sinceArg(since))
	if err != nil {
		return nil, err
	}
	shaFiles := map[string][]string{}
	type areaAgg struct {
		fixes, recent int
		seen          map[string]bool
		examples      []string
	}
	areas := map[string]*areaAgg{}
	for frows.Next() {
		var sha, path, when string
		if err := frows.Scan(&sha, &path, &when); err != nil {
			frows.Close()
			return nil, err
		}
		shaFiles[sha] = append(shaFiles[sha], path)
		dir := topDir(path)
		a := areas[dir]
		if a == nil {
			a = &areaAgg{seen: map[string]bool{}}
			areas[dir] = a
		}
		if !a.seen[sha] {
			a.seen[sha] = true
			a.fixes++
			if when >= recentThreshold {
				a.recent++
			}
			if len(a.examples) < 2 {
				a.examples = append(a.examples, sha[:min(8, len(sha))])
			}
		}
	}
	frows.Close()

	// hot_areas ordenadas.
	var hot []BugArea
	for dir, a := range areas {
		w := 0.0
		if a.fixes > 0 {
			w = float64(a.recent) / float64(a.fixes)
		}
		hot = append(hot, BugArea{Dir: dir, Fixes: a.fixes, RecentFixes: a.recent, RecencyWeight: w, Examples: a.examples})
	}
	sort.Slice(hot, func(i, j int) bool { return (hot[i].Fixes + hot[i].RecentFixes) > (hot[j].Fixes + hot[j].RecentFixes) })
	if len(hot) > limit {
		hot = hot[:limit]
	}

	// categorías por taxonomía: cada categoría con sus fixes, ficheros y ejemplos.
	var cats []BugCategory
	for cat, set := range catCommits {
		fileCount := map[string]int{}
		var examples []string
		for sha := range set {
			for _, p := range shaFiles[sha] {
				fileCount[p]++
			}
			if len(examples) < 2 {
				examples = append(examples, sha[:min(8, len(sha))])
			}
		}
		cats = append(cats, BugCategory{Category: cat, Fixes: len(set), TopFiles: topN(fileCount, 3), Examples: examples})
	}
	sort.Slice(cats, func(i, j int) bool { return cats[i].Fixes > cats[j].Fixes })
	if len(cats) > limit {
		cats = cats[:limit]
	}

	return map[string]any{"categories": cats, "hot_areas": hot, "total_fixes": len(fixes)}, nil
}

func topN(counts map[string]int, n int) []string {
	type kv struct {
		k string
		v int
	}
	var s []kv
	for k, v := range counts {
		s = append(s, kv{k, v})
	}
	sort.Slice(s, func(i, j int) bool { return s[i].v > s[j].v })
	var out []string
	for i := 0; i < len(s) && i < n; i++ {
		out = append(out, s[i].k)
	}
	return out
}

// cleanBody quita trailers de git (Co-authored-by, Signed-off-by…) y líneas de
// email/URL, que si no dominan las categorías con ruido ("authored", "com"…).
func cleanBody(body string) string {
	var b strings.Builder
	for _, line := range strings.Split(body, "\n") {
		l := strings.TrimSpace(line)
		if l == "" {
			continue
		}
		low := strings.ToLower(l)
		if trailerPrefix(low) || strings.Contains(low, "noreply") || strings.Contains(low, "://") {
			continue
		}
		b.WriteString(l)
		b.WriteByte('\n')
	}
	return b.String()
}

var trailers = []string{"co-authored-by:", "signed-off-by:", "reviewed-by:", "acked-by:",
	"tested-by:", "reported-by:", "cc:", "fixes:", "closes:", "refs:", "see-also:", "author:", "date:"}

func trailerPrefix(low string) bool {
	for _, t := range trailers {
		if strings.HasPrefix(low, t) {
			return true
		}
	}
	return false
}

func isDigits(s string) bool {
	for _, r := range s {
		if r < '0' || r > '9' {
			return false
		}
	}
	return len(s) > 0
}

// stopwords: ruido que no sirve como categoría (ES + EN + verbos de fix).
var stopwords = func() map[string]bool {
	m := map[string]bool{}
	es := `que los las del una uno por para con como este esta esto estos estas ese esa eso esos esas aquel sus sin asi así dos tres vez veces cada antes despues después todo toda todos todas hay ser estar tener hacer hace poner pone dar ver ir mismo misma mismos otras otro otra otros poco poca mucho mucha muy aun aún tan tanto solo sólo bien mal aqui aquí alli allí ahi ahí donde dónde cuando cuándo como cómo porque porqué pero sino aunque entonces luego ahora hoy ayer sea son era fue han has hemos sido siendo nuevo nueva nuevos version versión sobre entre desde hasta menos mas más ademas además ademas cual cuales cuál nada algo alguno alguna cabe segun según ante bajo tras esta este entre uso usa usar via par les nos les mientras`
	en := `the and for with not but you are was were will can use using used new old test tests wip merge branch feat feature refactor docs chore style perf build fix fixes fixed bug bugs this that these those when where what which who whom whose then than there their them they have has had been being does did done doing our your its from into onto out off over under about above below only just also same such each any all some more most less few via per get set add remove make made now here also not only after before while during since until because so if else while into within without across via com net org www github authored signed off off-by co ver`
	esFix := `arreglar arregla arreglo arreglado corregir corrige corregido correccion corrección soluciona solucion solución error errores falla fallo fallos problema problemas issue update actualiza actualizar anade añade agrega quita quitar cambia cambio elimina`
	for _, s := range []string{es, en, esFix} {
		for _, w := range strings.Fields(s) {
			m[w] = true
		}
	}
	return m
}()

// Ticket devuelve los commits y ficheros ligados a un ticket.
func Ticket(db *sql.DB, id string) (map[string]any, error) {
	rows, err := db.Query(`
		SELECT cm.sha, cm.subject FROM commit_tickets ct
		JOIN commits cm ON cm.sha=ct.commit_sha
		WHERE ct.ticket_id=? ORDER BY cm.authored_at`, id)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	type commitRef struct {
		SHA     string `json:"sha"`
		Subject string `json:"subject"`
	}
	var commits []commitRef
	for rows.Next() {
		var cr commitRef
		if err := rows.Scan(&cr.SHA, &cr.Subject); err != nil {
			return nil, err
		}
		cr.SHA = cr.SHA[:min(8, len(cr.SHA))]
		commits = append(commits, cr)
	}
	frows, err := db.Query(`
		SELECT DISTINCT f.path FROM commit_tickets ct
		JOIN changes c ON c.commit_sha=ct.commit_sha
		JOIN files f ON f.id=c.file_id
		WHERE ct.ticket_id=? LIMIT 50`, id)
	if err != nil {
		return nil, err
	}
	defer frows.Close()
	var files []string
	for frows.Next() {
		var p string
		if err := frows.Scan(&p); err != nil {
			return nil, err
		}
		files = append(files, p)
	}
	// PRs/issues del forge relacionados: por número (#N) o por mención en el título.
	prows, err := db.Query(`SELECT number, kind, title, state, labels FROM issues
		WHERE ('#'||number)=? OR title LIKE '%'||?||'%' ORDER BY number LIMIT 20`, id, id)
	var prs []map[string]any
	if err == nil {
		defer prows.Close()
		for prows.Next() {
			var num int
			var kind, title, state, labels string
			if err := prows.Scan(&num, &kind, &title, &state, &labels); err != nil {
				break
			}
			prs = append(prs, map[string]any{"number": num, "kind": kind, "title": title, "state": state, "labels": labels})
		}
	}
	return map[string]any{"ticket": id, "commits": commits, "files": files, "prs": prs}, nil
}

// PullRequests lista PRs del forge (los más recientes), con su estado y si es bug.
func PullRequests(db *sql.DB, since string, limit int) ([]map[string]any, error) {
	rows, err := db.Query(`SELECT number,title,state,merged,is_bug,labels FROM issues
		WHERE kind='pr' AND (closed_at >= ? OR closed_at='') ORDER BY number DESC LIMIT ?`, sinceArg(since), limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []map[string]any
	for rows.Next() {
		var num, merged, isBug int
		var title, state, labels string
		if err := rows.Scan(&num, &title, &state, &merged, &isBug, &labels); err != nil {
			return nil, err
		}
		out = append(out, map[string]any{"number": num, "title": title, "state": state,
			"merged": merged == 1, "is_bug": isBug == 1, "labels": labels})
	}
	return out, rows.Err()
}

// Phases lista las etiquetas como fases del proyecto.
func Phases(db *sql.DB) ([]map[string]string, error) {
	rows, err := db.Query(`SELECT name, tagged_at FROM tags ORDER BY tagged_at`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []map[string]string
	for rows.Next() {
		var name, date string
		if err := rows.Scan(&name, &date); err != nil {
			return nil, err
		}
		out = append(out, map[string]string{"tag": name, "date": date})
	}
	return out, rows.Err()
}

// Search: búsqueda de commits por texto. Usa FTS5 (BM25) si está; si no, LIKE.
func Search(db *sql.DB, hasFTS bool, query string, limit int) ([]map[string]any, error) {
	var rows *sql.Rows
	var err error
	if hasFTS {
		rows, err = db.Query(`SELECT c.sha, c.subject, c.authored_at
			FROM commits_fts f JOIN commits c ON c.sha=f.sha
			WHERE commits_fts MATCH ? ORDER BY rank LIMIT ?`, query, limit)
	} else {
		rows, err = db.Query(`SELECT sha, subject, authored_at FROM commits
			WHERE (subject||' '||COALESCE(body,'')) LIKE '%'||?||'%'
			ORDER BY authored_at DESC LIMIT ?`, query, limit)
	}
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []map[string]any
	for rows.Next() {
		var sha, subject, when string
		if err := rows.Scan(&sha, &subject, &when); err != nil {
			return nil, err
		}
		out = append(out, map[string]any{"sha": sha[:min(8, len(sha))], "subject": subject, "date": when[:min(10, len(when))]})
	}
	return out, rows.Err()
}

// Similar: commits casi-duplicados de `shaPrefix` por distancia de SimHash.
func Similar(db *sql.DB, shaPrefix string, maxDist, limit int) ([]map[string]any, error) {
	var target int64
	var tsha string
	err := db.QueryRow("SELECT sha, simhash FROM commits WHERE sha LIKE ?||'%' LIMIT 1", shaPrefix).Scan(&tsha, &target)
	if err != nil {
		return nil, err
	}
	rows, err := db.Query("SELECT sha, subject, simhash FROM commits WHERE sha != ?", tsha)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	type cand struct {
		sha, subject string
		dist         int
	}
	var cs []cand
	for rows.Next() {
		var sha, subject string
		var sh int64
		if err := rows.Scan(&sha, &subject, &sh); err != nil {
			return nil, err
		}
		d := simhash.Distance(uint64(target), uint64(sh))
		if d <= maxDist {
			cs = append(cs, cand{sha, subject, d})
		}
	}
	sort.Slice(cs, func(i, j int) bool { return cs[i].dist < cs[j].dist })
	if len(cs) > limit {
		cs = cs[:limit]
	}
	out := []map[string]any{}
	for _, c := range cs {
		out = append(out, map[string]any{"sha": c.sha[:min(8, len(c.sha))], "subject": c.subject, "distance": c.dist})
	}
	return out, nil
}

func topDir(path string) string {
	parts := strings.Split(path, "/")
	if len(parts) <= 1 {
		return "(raíz)"
	}
	if len(parts) >= 2 {
		return parts[0] + "/" + parts[1]
	}
	return parts[0]
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}
