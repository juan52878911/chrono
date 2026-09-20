// Package tracker es el adaptador del puerto Tracker: trae PRs e issues del
// forge (GitHub vía `gh`). Opcional: si no hay `gh` o remoto GitHub, se omite.
// Las labels del forge son la señal DETERMINISTA para "¿es un bug?".
package tracker

import (
	"context"
	"encoding/json"
	"fmt"
	"os/exec"
	"strings"
	"time"
)

// Item es un PR o un issue normalizado.
type Item struct {
	Number   int
	Kind     string // "pr" | "issue"
	Title    string
	State    string // OPEN | CLOSED | MERGED
	Labels   []string
	Merged   bool
	ClosedAt string
}

// Slug elige el repo GitHub a consultar: prefiere `upstream` (flujo de fork,
// donde viven los PRs), y si no, `origin`. Devuelve owner/repo y si es válido.
func Slug(repo string) (string, bool) {
	for _, remote := range []string{"upstream", "origin"} {
		out, err := exec.Command("git", "-C", repo, "remote", "get-url", remote).Output()
		if err != nil {
			continue
		}
		if strings.Contains(string(out), "github.com") {
			return ownerRepo(string(out)), true
		}
	}
	return "", false
}

// Available indica si hay `gh` autenticado y un remoto GitHub (upstream u origin).
func Available(repo string) bool {
	if _, err := exec.LookPath("gh"); err != nil {
		return false
	}
	if _, ok := Slug(repo); !ok {
		return false
	}
	return exec.Command("gh", "auth", "status").Run() == nil
}

func ownerRepo(remoteURL string) string {
	u := strings.TrimSpace(remoteURL)
	u = strings.TrimSuffix(u, ".git")
	if i := strings.Index(u, "github.com"); i >= 0 {
		u = u[i+len("github.com"):]
		u = strings.TrimLeft(u, ":/")
	}
	return u
}

// Fetch trae PRs e issues (todos los estados) del repo elegido. Devuelve también
// el slug consultado. NO pide `body` ni `commits`: en repos grandes eso revienta
// el límite de nodos de la API GraphQL de GitHub.
func Fetch(repo string, limit int) ([]Item, string, error) {
	nwo, ok := Slug(repo)
	if !ok {
		return nil, "", fmt.Errorf("no hay remoto GitHub")
	}
	var items []Item
	type ghItem struct {
		Number   int    `json:"number"`
		Title    string `json:"title"`
		State    string `json:"state"`
		MergedAt string `json:"mergedAt"`
		ClosedAt string `json:"closedAt"`
		Labels   []struct {
			Name string `json:"name"`
		} `json:"labels"`
	}
	parse := func(out []byte, kind string) {
		var arr []ghItem
		if json.Unmarshal(out, &arr) != nil {
			return
		}
		for _, x := range arr {
			it := Item{Number: x.Number, Kind: kind, Title: x.Title, State: x.State,
				Merged: x.State == "MERGED", ClosedAt: x.ClosedAt + x.MergedAt}
			for _, l := range x.Labels {
				it.Labels = append(it.Labels, l.Name)
			}
			items = append(items, it)
		}
	}
	prOut, prErr := ghJSON(nwo, "pr", limit, "number,title,state,mergedAt,labels")
	if prErr != nil {
		return nil, nwo, prErr
	}
	parse(prOut, "pr")
	isOut, isErr := ghJSON(nwo, "issue", limit, "number,title,state,closedAt,labels")
	if isErr != nil {
		// Issues pueden estar deshabilitados en el repo; no es fatal.
		return items, nwo, nil
	}
	parse(isOut, "issue")
	return items, nwo, nil
}

func ghJSON(nwo, kind string, limit int, fields string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()
	cmd := exec.CommandContext(ctx, "gh", kind, "list", "-R", nwo, "--state", "all",
		"--limit", itoa(limit), "--json", fields)
	out, err := cmd.Output()
	if err != nil {
		msg := err.Error()
		if ee, ok := err.(*exec.ExitError); ok && len(ee.Stderr) > 0 {
			msg = strings.TrimSpace(string(ee.Stderr))
		}
		if ctx.Err() == context.DeadlineExceeded {
			msg = "timeout (90s) consultando " + kind + "s"
		}
		return nil, fmt.Errorf("gh %s list: %s", kind, msg)
	}
	return out, nil
}

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	neg := n < 0
	if neg {
		n = -n
	}
	var b [20]byte
	i := len(b)
	for n > 0 {
		i--
		b[i] = byte('0' + n%10)
		n /= 10
	}
	if neg {
		i--
		b[i] = '-'
	}
	return string(b[i:])
}
