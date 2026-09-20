// Package report construye y serializa la respuesta según OUTPUT-CONTRACT.md.
package report

import (
	"encoding/json"
	"os"
	"time"

	"github.com/juan52878911/chrono/internal/store"
)

// Envelope es la envoltura común de toda respuesta.
type Envelope struct {
	SchemaVersion int            `json:"schema_version"`
	Question      string         `json:"question"`
	GeneratedAt   string         `json:"generated_at"`
	Window        map[string]any `json:"window"`
	Manifest      map[string]any `json:"manifest"`
	TokenBudget   map[string]any `json:"token_budget"`
	Result        any            `json:"result"`
}

// New arma el envelope leyendo el manifiesto del store.
func New(st *store.Store, question, since string, maxItems, returned int, result any) Envelope {
	man := map[string]any{}
	for _, k := range []string{"git_version", "first_parent", "bulk_threshold"} {
		if v, ok, _ := st.Meta(k); ok {
			man[k] = v
		}
	}
	var sinceVal any
	if since != "" {
		sinceVal = since
	}
	return Envelope{
		SchemaVersion: store.SchemaVersion,
		Question:      question,
		GeneratedAt:   time.Now().UTC().Format(time.RFC3339),
		Window:        map[string]any{"since": sinceVal, "until": nil},
		Manifest:      man,
		TokenBudget:   map[string]any{"max_items": maxItems, "returned": returned, "truncated": returned >= maxItems},
		Result:        result,
	}
}

// PrintJSON escribe el envelope como JSON indentado en stdout.
func PrintJSON(e Envelope) error {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(e)
}
