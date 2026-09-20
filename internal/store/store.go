// Package store es el adaptador del puerto Store: persiste el índice en un
// único archivo SQLite (relacional + grafo por CTEs; vectores llegan en v2).
package store

import (
	"database/sql"
	_ "embed"
	"path/filepath"
	"strings"

	"github.com/juan52878911/chrono/internal/domain"
	_ "modernc.org/sqlite" // driver puro Go, sin CGO.
)

//go:embed schema.sql
var schemaSQL string

// SchemaVersion se compara contra meta.schema_version al abrir.
const SchemaVersion = 1

// Tag es una etiqueta de git (para las "fases" del proyecto).
type Tag struct {
	Name string
	Date string
	SHA  string
}

// Store envuelve la conexión SQLite.
type Store struct {
	db     *sql.DB
	hasFTS bool // FTS5 disponible en este build de SQLite.
}

// HasFTS indica si la búsqueda por texto completo está disponible.
func (s *Store) HasFTS() bool { return s.hasFTS }

// Open abre (o crea) el índice y activa WAL para permitir lectura durante sync.
func Open(path string) (*Store, error) {
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, err
	}
	for _, p := range []string{"PRAGMA journal_mode=WAL", "PRAGMA busy_timeout=5000", "PRAGMA foreign_keys=ON"} {
		if _, err := db.Exec(p); err != nil {
			return nil, err
		}
	}
	return &Store{db: db}, nil
}

// DB expone la conexión para las consultas de métricas.
func (s *Store) DB() *sql.DB { return s.db }

// Close cierra la conexión.
func (s *Store) Close() error { return s.db.Close() }

// Migrate crea las tablas si no existen.
func (s *Store) Migrate() error {
	if _, err := s.db.Exec(schemaSQL); err != nil {
		return err
	}
	// Forward-compat: añade columnas nuevas a índices ya existentes (ignora si ya están).
	s.db.Exec("ALTER TABLE files ADD COLUMN excluded INTEGER NOT NULL DEFAULT 0")
	s.db.Exec("ALTER TABLE commits ADD COLUMN simhash INTEGER NOT NULL DEFAULT 0")
	// FTS5 es opcional: si el build de SQLite no lo trae, la búsqueda cae a LIKE.
	if _, err := s.db.Exec(`CREATE VIRTUAL TABLE IF NOT EXISTS commits_fts USING fts5(sha UNINDEXED, text)`); err == nil {
		s.hasFTS = true
	}
	return nil
}

// SetFastIngest activa PRAGMAs agresivos durante la carga (índice regenerable).
func (s *Store) SetFastIngest(on bool) {
	if on {
		s.db.Exec("PRAGMA synchronous=OFF")
		s.db.Exec("PRAGMA temp_store=MEMORY")
	} else {
		s.db.Exec("PRAGMA synchronous=NORMAL")
	}
}

// RebuildFTS reconstruye el índice de texto completo (barato; se hace al final).
func (s *Store) RebuildFTS() error {
	if !s.hasFTS {
		return nil
	}
	s.db.Exec("DELETE FROM commits_fts")
	_, err := s.db.Exec(`INSERT INTO commits_fts(sha,text)
		SELECT sha, subject||' '||COALESCE(body,'') FROM commits`)
	return err
}

// TopChangedPaths devuelve las rutas con más cambios (para dimensionar solo esas).
func (s *Store) TopChangedPaths(limit int) ([]string, error) {
	rows, err := s.db.Query(`SELECT f.path FROM changes c JOIN files f ON f.id=c.file_id
		GROUP BY f.id ORDER BY COUNT(*) DESC LIMIT ?`, limit)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []string
	for rows.Next() {
		var p string
		if err := rows.Scan(&p); err != nil {
			return nil, err
		}
		out = append(out, p)
	}
	return out, rows.Err()
}

// GetBlobLines devuelve el conteo de líneas cacheado para los OIDs pedidos.
func (s *Store) GetBlobLines(oids []string) (map[string]int, error) {
	res := map[string]int{}
	for _, oid := range oids {
		var n int
		if err := s.db.QueryRow("SELECT lines FROM blob_lines WHERE oid=?", oid).Scan(&n); err == nil {
			res[oid] = n
		}
	}
	return res, nil
}

// PutBlobLines guarda conteos de líneas recién calculados.
func (s *Store) PutBlobLines(m map[string]int) error {
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	for oid, n := range m {
		if _, err := tx.Exec("INSERT OR REPLACE INTO blob_lines(oid,lines) VALUES(?,?)", oid, n); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// Optimize compacta el índice y trunca el WAL al terminar la ingesta, para que
// el .db quede mínimo y no deje un -wal colgando al lado.
func (s *Store) Optimize() error {
	if _, err := s.db.Exec("VACUUM"); err != nil {
		return err
	}
	s.db.Exec("PRAGMA wal_checkpoint(TRUNCATE)")
	return nil
}

// MarkExcluded recalcula qué ficheros están excluidos por los globs de ruido.
func (s *Store) MarkExcluded(globs []string) error {
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.Exec("UPDATE files SET excluded=0"); err != nil {
		return err
	}
	if len(globs) == 0 {
		return tx.Commit()
	}
	rows, err := tx.Query("SELECT id, path FROM files")
	if err != nil {
		return err
	}
	var ids []int64
	for rows.Next() {
		var id int64
		var path string
		if err := rows.Scan(&id, &path); err != nil {
			rows.Close()
			return err
		}
		if matchAnyGlob(globs, path) {
			ids = append(ids, id)
		}
	}
	rows.Close()
	for _, id := range ids {
		if _, err := tx.Exec("UPDATE files SET excluded=1 WHERE id=?", id); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// matchAnyGlob: un glob casa contra el basename, la ruta completa, o (si es
// "dir/*") contra cualquier segmento de directorio.
func matchAnyGlob(globs []string, path string) bool {
	base := filepath.Base(path)
	for _, g := range globs {
		if ok, _ := filepath.Match(g, base); ok {
			return true
		}
		if ok, _ := filepath.Match(g, path); ok {
			return true
		}
		if strings.HasSuffix(g, "/*") {
			d := strings.TrimSuffix(g, "/*")
			for _, seg := range strings.Split(path, "/") {
				if seg == d {
					return true
				}
			}
		}
	}
	return false
}

// Meta lee un valor del manifiesto.
func (s *Store) Meta(key string) (string, bool, error) {
	var v string
	err := s.db.QueryRow("SELECT value FROM meta WHERE key=?", key).Scan(&v)
	if err == sql.ErrNoRows {
		return "", false, nil
	}
	return v, err == nil, err
}

// SetMeta escribe un valor del manifiesto.
func (s *Store) SetMeta(key, value string) error {
	_, err := s.db.Exec(
		"INSERT INTO meta(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
		key, value)
	return err
}

// Reset borra todos los datos (para reindex tras divergencia). Conserva meta.
func (s *Store) Reset() error {
	for _, t := range []string{"changes", "classifications", "commit_tickets", "tickets", "coupling", "commits", "files", "author_identities", "authors", "tags", "issues", "pr_commits"} {
		if _, err := s.db.Exec("DELETE FROM " + t); err != nil {
			return err
		}
	}
	return nil
}

// SetTags reemplaza la tabla de etiquetas.
func (s *Store) SetTags(tags []Tag) error {
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.Exec("DELETE FROM tags"); err != nil {
		return err
	}
	for _, t := range tags {
		if _, err := tx.Exec("INSERT OR REPLACE INTO tags(name,tagged_at,sha) VALUES(?,?,?)", t.Name, t.Date, t.SHA); err != nil {
			return err
		}
	}
	return tx.Commit()
}

// FinalizeFiles marca borrados y fija size_lines desde el estado de HEAD.
func (s *Store) FinalizeFiles(tracked map[string]int) error {
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.Exec("UPDATE files SET deleted=1"); err != nil {
		return err
	}
	for path, lines := range tracked {
		_, err := tx.Exec(`INSERT INTO files(path,size_lines,deleted) VALUES(?,?,0)
			ON CONFLICT(path) DO UPDATE SET size_lines=excluded.size_lines, deleted=0`, path, lines)
		if err != nil {
			return err
		}
	}
	return tx.Commit()
}

// Issue es un PR o issue del forge, listo para persistir.
type Issue struct {
	Number   int
	Kind     string
	Title    string
	State    string
	Labels   []string
	IsBug    bool
	Merged   bool
	Author   string
	ClosedAt string
	Commits  []string
}

// UpsertIssues reemplaza PRs/issues y sus enlaces a commits. bugLabels decide is_bug.
func (s *Store) UpsertIssues(items []Issue, bugLabels []string) error {
	tx, err := s.db.Begin()
	if err != nil {
		return err
	}
	defer tx.Rollback()
	tx.Exec("DELETE FROM issues")
	tx.Exec("DELETE FROM pr_commits")
	for _, it := range items {
		b2i := func(b bool) int {
			if b {
				return 1
			}
			return 0
		}
		isBug := labelsMatch(it.Labels, bugLabels)
		_, err := tx.Exec(`INSERT OR REPLACE INTO issues(number,kind,title,state,labels,is_bug,merged,author,closed_at)
			VALUES(?,?,?,?,?,?,?,?,?)`, it.Number, it.Kind, it.Title, it.State,
			strings.Join(it.Labels, ","), b2i(isBug), b2i(it.Merged), it.Author, it.ClosedAt)
		if err != nil {
			return err
		}
		for _, sha := range it.Commits {
			tx.Exec("INSERT OR IGNORE INTO pr_commits(number,commit_sha) VALUES(?,?)", it.Number, sha)
		}
	}
	// Enlace PR/issue -> commit por referencia #N en el mensaje (determinista).
	tx.Exec(`INSERT OR IGNORE INTO pr_commits(number,commit_sha)
		SELECT i.number, ct.commit_sha FROM commit_tickets ct
		JOIN issues i ON ('#'||i.number)=ct.ticket_id`)
	return tx.Commit()
}

func labelsMatch(labels, bug []string) bool {
	for _, l := range labels {
		ll := strings.ToLower(l)
		for _, b := range bug {
			if strings.Contains(ll, strings.ToLower(b)) {
				return true
			}
		}
	}
	return false
}

// EnrichFromForge sube a determinista (source=forge, confianza 1.0) los fixes
// respaldados por una label de bug del forge: commits de un PR/issue bug, o
// commits que referencian (#N) un issue bug. Devuelve cuántos marcó.
func (s *Store) EnrichFromForge() (int, error) {
	q := `UPDATE classifications SET is_fix=1, kind='fix', source='forge', confidence=1.0
	      WHERE commit_sha IN (
	        SELECT pc.commit_sha FROM pr_commits pc JOIN issues i ON i.number=pc.number WHERE i.is_bug=1
	        UNION
	        SELECT ct.commit_sha FROM commit_tickets ct JOIN issues i ON ('#'||i.number)=ct.ticket_id WHERE i.is_bug=1
	      )`
	res, err := s.db.Exec(q)
	if err != nil {
		return 0, err
	}
	n, _ := res.RowsAffected()
	return int(n), nil
}

// Writer acumula una ingesta en una sola transacción con cachés en memoria.
type Writer struct {
	tx      *sql.Tx
	authors map[string]int64
	files   map[string]int64
}

// NewWriter abre la transacción de ingesta.
func (s *Store) NewWriter() (*Writer, error) {
	tx, err := s.db.Begin()
	if err != nil {
		return nil, err
	}
	return &Writer{tx: tx, authors: map[string]int64{}, files: map[string]int64{}}, nil
}

func (w *Writer) authorID(name, email string) (int64, error) {
	if id, ok := w.authors[email]; ok {
		return id, nil
	}
	if _, err := w.tx.Exec("INSERT OR IGNORE INTO authors(canonical_email,display_name) VALUES(?,?)", email, name); err != nil {
		return 0, err
	}
	var id int64
	if err := w.tx.QueryRow("SELECT id FROM authors WHERE canonical_email=?", email).Scan(&id); err != nil {
		return 0, err
	}
	if _, err := w.tx.Exec("INSERT OR IGNORE INTO author_identities(email,name,author_id) VALUES(?,?,?)", email, name, id); err != nil {
		return 0, err
	}
	w.authors[email] = id
	return id, nil
}

func (w *Writer) fileID(path string) (int64, error) {
	if id, ok := w.files[path]; ok {
		return id, nil
	}
	if _, err := w.tx.Exec("INSERT OR IGNORE INTO files(path) VALUES(?)", path); err != nil {
		return 0, err
	}
	var id int64
	if err := w.tx.QueryRow("SELECT id FROM files WHERE path=?", path).Scan(&id); err != nil {
		return 0, err
	}
	w.files[path] = id
	return id, nil
}

// AddCommit inserta un commit con sus cambios, clasificación y tickets.
func (w *Writer) AddCommit(rev domain.Revision, cls domain.Classification, tickets []string) error {
	aid, err := w.authorID(rev.AuthorName, rev.AuthorEmail)
	if err != nil {
		return err
	}
	bulk := 0
	if rev.IsBulk {
		bulk = 1
	}
	_, err = w.tx.Exec(`INSERT OR REPLACE INTO commits(sha,author_id,authored_at,subject,body,simhash,is_bulk,files_touched)
		VALUES(?,?,?,?,?,?,?,?)`, rev.SHA, aid, rev.When, rev.Subject, rev.Body, int64(rev.Simhash), bulk, len(rev.Changes))
	if err != nil {
		return err
	}
	for _, ch := range rev.Changes {
		fid, err := w.fileID(ch.Path)
		if err != nil {
			return err
		}
		_, err = w.tx.Exec(`INSERT OR REPLACE INTO changes(commit_sha,file_id,change_type,old_path,added,deleted)
			VALUES(?,?,?,?,?,?)`, rev.SHA, fid, string(ch.Type), ch.OldPath, ch.Added, ch.Deleted)
		if err != nil {
			return err
		}
	}
	b2i := func(b bool) int {
		if b {
			return 1
		}
		return 0
	}
	_, err = w.tx.Exec(`INSERT OR REPLACE INTO classifications(commit_sha,kind,is_fix,is_revert,severity,confidence,source)
		VALUES(?,?,?,?,?,?,?)`, rev.SHA, cls.Kind, b2i(cls.IsFix), b2i(cls.IsRevert), cls.Severity, cls.Confidence, cls.Source)
	if err != nil {
		return err
	}
	for _, t := range tickets {
		if _, err := w.tx.Exec("INSERT OR IGNORE INTO tickets(id) VALUES(?)", t); err != nil {
			return err
		}
		if _, err := w.tx.Exec("INSERT OR IGNORE INTO commit_tickets(commit_sha,ticket_id) VALUES(?,?)", rev.SHA, t); err != nil {
			return err
		}
	}
	return nil
}

// Commit cierra la transacción de ingesta.
func (w *Writer) Commit() error { return w.tx.Commit() }

// Rollback aborta la transacción de ingesta.
func (w *Writer) Rollback() error { return w.tx.Rollback() }
