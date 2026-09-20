// Package ingest es el adaptador del puerto Source: lee git y lo traduce a
// domain.Revision, escribiéndolo en el store. En v1 solo hay adaptador git
// (shell-out en streaming).
package ingest

import (
	"bufio"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/juan52878911/chrono/internal/classify"
	"github.com/juan52878911/chrono/internal/config"
	"github.com/juan52878911/chrono/internal/domain"
	"github.com/juan52878911/chrono/internal/i18n"
	"github.com/juan52878911/chrono/internal/simhash"
	"github.com/juan52878911/chrono/internal/store"
	"github.com/juan52878911/chrono/internal/tracker"
)

// sizeCap: nº máximo de ficheros a los que se les calcula el tamaño (los más
// cambiados). En repos enormes evita leer el contenido de miles de ficheros.
const sizeCap = 4000

// BulkThreshold: commits que tocan más ficheros se marcan IsBulk y se excluyen
// del acoplamiento (evita la explosión O(ficheros²)). Grabado en el manifiesto.
const BulkThreshold = 50

// Formato: %x1e separa registros; %x1f separa campos. El %x1f final tras %b
// aísla el cuerpo del bloque numstat que git añade después.
const logFormat = "--pretty=format:%x1e%H%x1f%an%x1f%ae%x1f%aI%x1f%s%x1f%b%x1f"

// IsShallow indica si el repo es un clon shallow (métricas serían falsas).
func IsShallow(repo string) (bool, error) {
	out, err := exec.Command("git", "-C", repo, "rev-parse", "--is-shallow-repository").Output()
	if err != nil {
		return false, err
	}
	return strings.TrimSpace(string(out)) == "true", nil
}

func headSHA(repo string) (string, error) {
	out, err := exec.Command("git", "-C", repo, "rev-parse", "HEAD").Output()
	return strings.TrimSpace(string(out)), err
}

func commitExists(repo, sha string) bool {
	return exec.Command("git", "-C", repo, "cat-file", "-e", sha+"^{commit}").Run() == nil
}

// Run hace la ingesta completa (primera pasada). reset borra el índice previo.
func Run(repo string, st *store.Store, reset bool) (int, error) {
	if sh, err := IsShallow(repo); err != nil {
		return 0, fmt.Errorf("checking shallow: %w", err)
	} else if sh {
		return 0, fmt.Errorf("%s", i18n.T(
			"the repo is a shallow clone (limited depth): metrics would be false. Clone it fully",
			"el repo es un clon shallow (depth limitado): las métricas serían falsas. Clona completo"))
	}
	if reset {
		if err := st.Reset(); err != nil {
			return 0, err
		}
	}
	cfg := config.Load(repo)
	cl := classify.New(cfg)
	n, err := ingestRange(repo, st, cl, nil)
	if err != nil {
		return n, err
	}
	if err := finalize(repo, st, cfg); err != nil {
		return n, err
	}
	return n, nil
}

// Sync procesa solo el delta desde la marca de agua. Si el último SHA ya no
// existe (rebase/force-push) reconstruye.
func Sync(repo string, st *store.Store) (int, error) {
	last, ok, err := storeMeta(st, "last_sha")
	if err != nil {
		return 0, err
	}
	if !ok || last == "" {
		return Run(repo, st, false)
	}
	if !commitExists(repo, last) {
		fmt.Fprintln(os.Stderr, i18n.T(
			"chrono: divergence detected (rebase/force-push) -> full reindex",
			"chrono: divergencia detectada (rebase/force-push) -> reindex completo"))
		return Run(repo, st, true)
	}
	head, _ := headSHA(repo)
	if head == last {
		return 0, nil // nada nuevo.
	}
	cfg := config.Load(repo)
	cl := classify.New(cfg)
	n, err := ingestRange(repo, st, cl, []string{last + "..HEAD"})
	if err != nil {
		return n, err
	}
	if err := finalize(repo, st, cfg); err != nil {
		return n, err
	}
	return n, nil
}

func storeMeta(st *store.Store, key string) (string, bool, error) { return st.Meta(key) }

func ingestRange(repo string, st *store.Store, cl *classify.Classifier, extra []string) (int, error) {
	// --no-merges (sin --first-parent): captura TODO el trabajo real, incluidos
	// los commits de rama que sí cambian ficheros. --first-parent ocultaría ese
	// trabajo en repos con muchos merges (subcuenta). Ver DECISIONS.md.
	args := []string{"-C", repo, "log", "--no-merges", "--use-mailmap", "--numstat", "-M", logFormat}
	args = append(args, extra...)
	cmd := exec.Command("git", args...)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return 0, err
	}
	if err := cmd.Start(); err != nil {
		return 0, err
	}
	st.SetFastIngest(true)
	defer st.SetFastIngest(false)
	w, err := st.NewWriter()
	if err != nil {
		return 0, err
	}
	scan := bufio.NewScanner(stdout)
	scan.Buffer(make([]byte, 1024*1024), 64*1024*1024)
	scan.Split(splitRecords)
	count := 0
	for scan.Scan() {
		rec := scan.Text()
		if strings.TrimSpace(rec) == "" {
			continue
		}
		rev, ok := parseRecord(rec)
		if !ok {
			continue
		}
		toks := simhash.Tokenize(rev.Subject, rev.Body)
		for _, ch := range rev.Changes {
			toks = append(toks, simhash.Tokenize(ch.Path)...)
		}
		rev.Simhash = simhash.Of(toks)
		cls := cl.Classify(rev.Subject, rev.Body)
		tickets := cl.Tickets(rev.Subject, rev.Body)
		if err := w.AddCommit(rev, cls, tickets); err != nil {
			w.Rollback()
			return count, err
		}
		count++
		if count%2000 == 0 {
			fmt.Fprintf(os.Stderr, "\r  %d commits…", count)
		}
	}
	if count >= 2000 {
		fmt.Fprintf(os.Stderr, "\r  %d commits\n", count)
	}
	if err := scan.Err(); err != nil {
		w.Rollback()
		return count, err
	}
	if err := w.Commit(); err != nil {
		return count, err
	}
	if err := cmd.Wait(); err != nil {
		return count, fmt.Errorf("git log: %w", err)
	}
	return count, nil
}

// splitRecords divide el flujo por el separador de registro \x1e.
func splitRecords(data []byte, atEOF bool) (advance int, token []byte, err error) {
	for i := 0; i < len(data); i++ {
		if data[i] == 0x1e {
			if i == 0 {
				return 1, nil, nil // salta separador inicial.
			}
			return i + 1, data[:i], nil
		}
	}
	if atEOF && len(data) > 0 {
		return len(data), data, nil
	}
	return 0, nil, nil
}

func parseRecord(rec string) (domain.Revision, bool) {
	parts := strings.Split(rec, "\x1f")
	if len(parts) < 6 {
		return domain.Revision{}, false
	}
	rev := domain.Revision{
		SHA:         parts[0],
		AuthorName:  parts[1],
		AuthorEmail: parts[2],
		When:        parts[3],
		Subject:     parts[4],
		Body:        strings.TrimSpace(parts[5]),
	}
	if len(parts) >= 7 {
		rev.Changes = parseNumstat(parts[6])
	}
	rev.IsBulk = len(rev.Changes) > BulkThreshold
	return rev, true
}

func parseNumstat(block string) []domain.FileChange {
	var out []domain.FileChange
	for _, line := range strings.Split(block, "\n") {
		line = strings.TrimRight(line, "\r")
		if line == "" {
			continue
		}
		cols := strings.SplitN(line, "\t", 3)
		if len(cols) != 3 {
			continue
		}
		ch := domain.FileChange{Type: domain.Modified}
		if cols[0] == "-" || cols[1] == "-" {
			ch.IsBinary = true
			ch.Type = domain.Binary
			ch.Added, ch.Deleted = -1, -1
		} else {
			ch.Added, _ = strconv.Atoi(cols[0])
			ch.Deleted, _ = strconv.Atoi(cols[1])
		}
		newP, oldP, isRename := splitRename(cols[2])
		ch.Path = newP
		if isRename {
			ch.OldPath = oldP
			ch.Type = domain.Renamed
		}
		out = append(out, ch)
	}
	return out
}

// splitRename interpreta las notaciones de rename de numstat.
func splitRename(p string) (newP, oldP string, isRename bool) {
	if !strings.Contains(p, " => ") {
		return p, "", false
	}
	if i := strings.Index(p, "{"); i >= 0 {
		if j := strings.Index(p, "}"); j > i {
			mid := p[i+1 : j]
			pre, post := p[:i], p[j+1:]
			seg := strings.SplitN(mid, " => ", 2)
			if len(seg) == 2 {
				oldP = filepath.Clean(pre + seg[0] + post)
				newP = filepath.Clean(pre + seg[1] + post)
				return newP, oldP, true
			}
		}
	}
	seg := strings.SplitN(p, " => ", 2)
	if len(seg) == 2 {
		return strings.TrimSpace(seg[1]), strings.TrimSpace(seg[0]), true
	}
	return p, "", false
}

func finalize(repo string, st *store.Store, cfg config.Config) error {
	fmt.Fprintln(os.Stderr, i18n.T("  computing file sizes…", "  calculando tamaños de fichero…"))
	tracked, err := trackedFiles(repo, st)
	if err != nil {
		return err
	}
	if err := st.FinalizeFiles(tracked); err != nil {
		return err
	}
	if err := st.MarkExcluded(cfg.ExcludeGlobs); err != nil {
		return err
	}
	fmt.Fprintln(os.Stderr, i18n.T("  building search index…", "  construyendo índice de búsqueda…"))
	if err := st.RebuildFTS(); err != nil {
		return err
	}
	if tags, err := gitTags(repo); err != nil {
		fmt.Fprintln(os.Stderr, i18n.T("  warning: could not read tags: ", "  aviso: no se pudieron leer etiquetas: ")+err.Error())
	} else if err := st.SetTags(tags); err != nil {
		return fmt.Errorf("saving tags: %w", err)
	}
	// Puerto Tracker (opcional): PRs/issues del forge (upstream si es un fork).
	switch tracker.Check(repo) {
	case tracker.NoGh:
		fmt.Fprintln(os.Stderr, i18n.T(
			"  warning: 'gh' not found — skipping PRs/issues (bug-by-label). Install GitHub CLI to enable: https://cli.github.com",
			"  aviso: no está 'gh' — se omiten PRs/issues (bug por label). Instala GitHub CLI para activarlo: https://cli.github.com"))
	case tracker.NoRemote:
		fmt.Fprintln(os.Stderr, i18n.T(
			"  warning: no GitHub remote (upstream/origin) — skipping PRs/issues.",
			"  aviso: no hay remoto GitHub (upstream/origin) — se omiten PRs/issues."))
	case tracker.NoAuth:
		fmt.Fprintln(os.Stderr, i18n.T(
			"  warning: 'gh' is not authenticated — skipping PRs/issues. Run: gh auth login",
			"  aviso: 'gh' no está autenticado — se omiten PRs/issues. Ejecuta: gh auth login"))
	case tracker.OK:
		fmt.Fprintln(os.Stderr, i18n.T("  fetching PRs/issues…", "  trayendo PRs/issues…"))
		items, nwo, err := tracker.Fetch(repo, 1000)
		if err != nil {
			fmt.Fprintln(os.Stderr, i18n.T("  note: forge skipped: ", "  aviso: forge omitido: ")+err.Error())
		} else {
			issues := make([]store.Issue, len(items))
			for i, it := range items {
				issues[i] = store.Issue{Number: it.Number, Kind: it.Kind, Title: it.Title,
					State: it.State, Labels: it.Labels, Merged: it.Merged, ClosedAt: it.ClosedAt}
			}
			if err := st.UpsertIssues(issues, cfg.BugLabels); err != nil {
				fmt.Fprintln(os.Stderr, i18n.T("  warning: could not store PRs/issues: ", "  aviso: no se pudieron guardar PRs/issues: ")+err.Error())
			} else {
				n, _ := st.EnrichFromForge()
				fmt.Fprintf(os.Stderr, i18n.T("  forge: %d PRs/issues from %s (%d fixes by label)\n", "  forge: %d PRs/issues de %s (%d fixes por label)\n"), len(items), nwo, n)
			}
		}
	}

	gv, _ := exec.Command("git", "--version").Output()
	head, _ := headSHA(repo)
	meta := map[string]string{
		"schema_version": strconv.Itoa(store.SchemaVersion),
		"git_version":    strings.TrimSpace(string(gv)),
		"first_parent":   "false",
		"no_merges":      "true",
		"mailmap_used":   "true",
		"bulk_threshold": strconv.Itoa(BulkThreshold),
		"last_sha":       head,
	}
	for k, v := range meta {
		if err := st.SetMeta(k, v); err != nil {
			return err
		}
	}
	return st.Optimize()
}

// trackedFiles devuelve ruta->líneas para HEAD. Solo calcula el tamaño de los
// ficheros MÁS CAMBIADOS (top sizeCap), que son los que hotspots necesita; el
// resto queda en 0. Cachea por OID de blob, así el sync solo lee blobs nuevos.
func trackedFiles(repo string, st *store.Store) (map[string]int, error) {
	pathOID, err := lsTree(repo)
	if err != nil {
		return nil, err
	}
	res := make(map[string]int, len(pathOID))
	for p := range pathOID {
		res[p] = 0 // presente en HEAD (no borrado), sin tamaño aún.
	}
	// Solo dimensionamos los ficheros más cambiados que siguen en HEAD.
	top, err := st.TopChangedPaths(sizeCap)
	if err != nil {
		return nil, err
	}
	oidSet := map[string]bool{}
	for _, p := range top {
		if oid, ok := pathOID[p]; ok {
			oidSet[oid] = true
		}
	}
	oids := make([]string, 0, len(oidSet))
	for o := range oidSet {
		oids = append(oids, o)
	}
	cached, _ := st.GetBlobLines(oids)
	var miss []string
	for _, o := range oids {
		if _, ok := cached[o]; !ok {
			miss = append(miss, o)
		}
	}
	if len(miss) > 0 {
		fresh, err := blobLines(repo, miss)
		if err != nil {
			return nil, err
		}
		st.PutBlobLines(fresh)
		for o, n := range fresh {
			cached[o] = n
		}
	}
	for _, p := range top {
		if oid, ok := pathOID[p]; ok {
			res[p] = cached[oid]
		}
	}
	return res, nil
}

// lsTree devuelve ruta->OID de blob de HEAD (sin leer contenido; barato).
func lsTree(repo string) (map[string]string, error) {
	out, err := exec.Command("git", "-C", repo, "ls-tree", "-r", "-z", "HEAD").Output()
	if err != nil {
		return nil, err
	}
	pathOID := map[string]string{}
	for _, rec := range strings.Split(string(out), "\x00") {
		if rec == "" {
			continue
		}
		tab := strings.IndexByte(rec, '\t')
		if tab < 0 {
			continue
		}
		meta := strings.Fields(rec[:tab])
		if len(meta) < 3 {
			continue
		}
		pathOID[rec[tab+1:]] = meta[2]
	}
	return pathOID, nil
}

// blobLines cuenta líneas de cada OID vía `git cat-file --batch` (un proceso).
func blobLines(repo string, oids []string) (map[string]int, error) {
	cmd := exec.Command("git", "-C", repo, "cat-file", "--batch")
	cmd.Stdin = strings.NewReader(strings.Join(oids, "\n") + "\n")
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	br := bufio.NewReaderSize(stdout, 1<<20)
	res := map[string]int{}
	for {
		header, err := br.ReadString('\n')
		if err != nil {
			break
		}
		f := strings.Fields(strings.TrimRight(header, "\n"))
		if len(f) < 3 { // "<oid> missing" u otra cosa
			continue
		}
		oid, size := f[0], f[2]
		n := atoi(size)
		buf := make([]byte, n+1) // +1 por el \n final que añade git
		io.ReadFull(br, buf)
		lines, hasNUL := 0, false
		for i := 0; i < n; i++ {
			if buf[i] == '\n' {
				lines++
			} else if buf[i] == 0 {
				hasNUL = true
			}
		}
		if hasNUL {
			lines = 0 // binario
		} else if n > 0 && buf[n-1] != '\n' {
			lines++ // última línea sin salto
		}
		res[oid] = lines
	}
	cmd.Wait()
	return res, nil
}

func atoi(s string) int {
	n := 0
	for _, c := range s {
		if c < '0' || c > '9' {
			break
		}
		n = n*10 + int(c-'0')
	}
	return n
}

func gitTags(repo string) ([]store.Tag, error) {
	out, err := exec.Command("git", "-C", repo, "for-each-ref",
		"--sort=creatordate",
		"--format=%(refname:short)\x1f%(creatordate:short)\x1f%(objectname:short)",
		"refs/tags").Output()
	if err != nil {
		return nil, err
	}
	var tags []store.Tag
	for _, line := range strings.Split(strings.TrimSpace(string(out)), "\n") {
		if line == "" {
			continue
		}
		c := strings.Split(line, "\x1f")
		if len(c) == 3 {
			tags = append(tags, store.Tag{Name: c[0], Date: c[1], SHA: c[2]})
		}
	}
	return tags, nil
}
