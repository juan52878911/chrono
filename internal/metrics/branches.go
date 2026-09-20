package metrics

// branches.go responde sobre las ramas leyendo git EN VIVO (no el índice): el
// índice solo conoce HEAD, pero las ramas cambian a cada rato y no merece la
// pena reindexar para saber su estado. Da, por rama: punta, antigüedad,
// ahead/behind respecto a la base, si está mergeada o stale, y sus autores.

import (
	"os/exec"
	"sort"
	"strconv"
	"strings"
	"time"
)

// staleDays: sin commits en este tiempo, la rama se marca como stale.
const staleDays = 90

// BranchTip es el commit en la punta de una rama.
type BranchTip struct {
	SHA     string `json:"sha"`
	Author  string `json:"author"`
	Date    string `json:"date"`
	Subject string `json:"subject"`
}

// BranchAuthor cuenta commits de un autor en los commits exclusivos de la rama.
type BranchAuthor struct {
	Name    string `json:"name"`
	Commits int    `json:"commits"`
}

// Branch resume el estado de una rama respecto a la base.
type Branch struct {
	Name      string         `json:"name"`
	Current   bool           `json:"current"`
	Tip       BranchTip      `json:"tip"`
	AgeDays   int            `json:"age_days"`
	Ahead     int            `json:"ahead"`  // commits en la rama que no están en la base.
	Behind    int            `json:"behind"` // commits en la base que no están en la rama.
	Merged    bool           `json:"merged"` // toda la rama está ya en la base.
	Stale     bool           `json:"stale"`  // sin actividad en staleDays días.
	Authors   []BranchAuthor `json:"authors,omitempty"`
	BusFactor int            `json:"bus_factor"`
}

// Branches devuelve el estado de las ramas locales respecto a una base. Si base
// está vacía, elige main, luego master, luego la rama actual.
func Branches(repo, base string) (string, string, []Branch, error) {
	current := gitLine(repo, "rev-parse", "--abbrev-ref", "HEAD")
	if base == "" {
		base = pickBase(repo, current)
	}

	// for-each-ref: una línea por rama con punta, fecha, autor y asunto.
	const fmtRef = "%(refname:short)\x1f%(objectname:short)\x1f%(committerdate:iso8601)\x1f%(authorname)\x1f%(contents:subject)"
	out, err := exec.Command("git", "-C", repo, "for-each-ref",
		"--format="+fmtRef, "refs/heads").Output()
	if err != nil {
		return base, current, nil, err
	}

	var branches []Branch
	for _, line := range strings.Split(strings.TrimRight(string(out), "\n"), "\n") {
		if line == "" {
			continue
		}
		p := strings.Split(line, "\x1f")
		if len(p) < 5 {
			continue
		}
		name := p[0]
		b := Branch{
			Name:    name,
			Current: name == current,
			Tip:     BranchTip{SHA: p[1], Date: p[2], Author: p[3], Subject: p[4]},
			AgeDays: ageDays(p[2]),
		}
		b.Stale = b.AgeDays >= staleDays
		// ahead/behind: rev-list --left-right --count base...rama → "behind\tahead".
		if name != base {
			if lr := gitLine(repo, "rev-list", "--left-right", "--count", base+"..."+name); lr != "" {
				fields := strings.Fields(lr)
				if len(fields) == 2 {
					b.Behind, _ = strconv.Atoi(fields[0])
					b.Ahead, _ = strconv.Atoi(fields[1])
				}
			}
			b.Merged = b.Ahead == 0
			if b.Ahead > 0 {
				b.Authors, b.BusFactor = branchAuthors(repo, base, name)
			}
		} else {
			b.Merged = true // la base está mergeada consigo misma por definición.
		}
		branches = append(branches, b)
	}

	// Orden: la actual primero; luego por actividad reciente (punta más nueva).
	sort.SliceStable(branches, func(i, j int) bool {
		if branches[i].Current != branches[j].Current {
			return branches[i].Current
		}
		return branches[i].Tip.Date > branches[j].Tip.Date
	})
	return base, current, branches, nil
}

// pickBase elige la rama base: main, master, o la actual como último recurso.
func pickBase(repo, current string) string {
	for _, cand := range []string{"main", "master"} {
		if exec.Command("git", "-C", repo, "rev-parse", "--verify", "-q", "refs/heads/"+cand).Run() == nil {
			return cand
		}
	}
	return current
}

// branchAuthors cuenta autores de los commits exclusivos de la rama (base..rama)
// y calcula un bus factor: cuántos autores acumulan >50% de esos commits.
func branchAuthors(repo, base, name string) ([]BranchAuthor, int) {
	out, err := exec.Command("git", "-C", repo, "shortlog", "-sn", "--no-merges", base+".."+name).Output()
	if err != nil {
		return nil, 0
	}
	var authors []BranchAuthor
	total := 0
	for _, line := range strings.Split(strings.TrimRight(string(out), "\n"), "\n") {
		line = strings.TrimSpace(line)
		if line == "" {
			continue
		}
		fields := strings.SplitN(line, "\t", 2)
		if len(fields) != 2 {
			continue
		}
		n, _ := strconv.Atoi(strings.TrimSpace(fields[0]))
		authors = append(authors, BranchAuthor{Name: fields[1], Commits: n})
		total += n
	}
	bus := 0
	acc := 0
	for _, a := range authors {
		bus++
		acc += a.Commits
		if total > 0 && acc*2 > total {
			break
		}
	}
	return authors, bus
}

// gitLine ejecuta git y devuelve la primera línea recortada (o "").
func gitLine(repo string, args ...string) string {
	full := append([]string{"-C", repo}, args...)
	out, err := exec.Command("git", full...).Output()
	if err != nil {
		return ""
	}
	return strings.TrimSpace(string(out))
}

// ageDays traduce una fecha ISO del commit a días desde hoy.
func ageDays(iso string) int {
	// git da p.ej. "2026-08-30 12:00:00 +0200"; probamos varios formatos.
	for _, layout := range []string{"2006-01-02 15:04:05 -0700", time.RFC3339, "2006-01-02"} {
		if t, err := time.Parse(layout, strings.TrimSpace(iso)); err == nil {
			d := int(time.Since(t).Hours() / 24)
			if d < 0 {
				d = 0
			}
			return d
		}
	}
	return 0
}
