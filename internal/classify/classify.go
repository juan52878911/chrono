// Package classify es el adaptador Nivel 0 del puerto Classifier: reglas y
// regex sobre el mensaje, ahora CONFIGURABLES por repo (reproducible y explícito).
// Los reverts se detectan por señal git-nativa (determinista), no por prosa.
package classify

import (
	"regexp"
	"strings"

	"github.com/juan52878911/chrono/internal/config"
	"github.com/juan52878911/chrono/internal/domain"
)

// convRe reconoce conventional commits (determinista y universal).
var convRe = regexp.MustCompile(`^(feat|fix|refactor|docs|chore|test|style|perf|build|ci)(\([^)]*\))?!?:`)

// revertBodyRe es la señal git-nativa de un revert: git la escribe siempre igual.
var revertBodyRe = regexp.MustCompile(`(?im)^\s*this reverts commit\s+([0-9a-f]{7,40})`)
var revertSubjRe = regexp.MustCompile(`(?i)^revert[:\s"]`)

// Classifier compila las reglas una vez a partir de la config del repo.
type Classifier struct {
	fix     *regexp.Regexp
	tickets []*regexp.Regexp
}

// New construye el clasificador desde la config.
func New(cfg config.Config) *Classifier {
	c := &Classifier{}
	if len(cfg.FixKeywords) > 0 {
		esc := make([]string, len(cfg.FixKeywords))
		for i, k := range cfg.FixKeywords {
			esc[i] = regexp.QuoteMeta(k)
		}
		c.fix = regexp.MustCompile(`(?i)(` + strings.Join(esc, "|") + `)`)
	}
	for _, p := range cfg.TicketPatterns {
		if re, err := regexp.Compile(p); err == nil {
			c.tickets = append(c.tickets, re)
		}
	}
	return c
}

// Classify aplica las reglas Nivel 0.
func (c *Classifier) Classify(subject, body string) domain.Classification {
	s := strings.TrimSpace(subject)
	cl := domain.Classification{Source: "rules", Confidence: 0.5}

	// Revert: señal determinista (cuerpo git-nativo tiene prioridad sobre el asunto).
	cl.IsRevert = revertBodyRe.MatchString(body) || revertSubjRe.MatchString(s)

	kind := "other"
	if m := convRe.FindStringSubmatch(s); m != nil {
		kind = m[1]
		if kind == "fix" {
			cl.Confidence = 0.9 // conventional commit: señal fuerte.
		}
	} else if c.fix != nil && (c.fix.MatchString(s) || c.fix.MatchString(body)) {
		kind = "fix"
	}
	if cl.IsRevert {
		kind = "fix"
		cl.Confidence = 0.9
	}
	cl.Kind = kind
	cl.IsFix = kind == "fix"
	return cl
}

// Tickets extrae ids de ticket según los patrones configurados.
func (c *Classifier) Tickets(subject, body string) []string {
	text := subject + "\n" + body
	seen := map[string]bool{}
	var out []string
	for _, re := range c.tickets {
		for _, m := range re.FindAllStringSubmatch(text, -1) {
			id := m[0]
			if len(m) > 1 && m[1] != "" {
				id = m[1]
			}
			id = strings.TrimSpace(id)
			if id != "" && !seen[id] {
				seen[id] = true
				out = append(out, id)
			}
		}
	}
	return out
}
