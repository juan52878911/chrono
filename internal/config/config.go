// Package config carga la configuración por repo desde .chrono/config.json.
// Hace la clasificación EXPLÍCITA y reproducible: las reglas dejan de ser una
// suposición oculta y pasan a ser un artefacto versionable que el usuario ajusta.
package config

import (
	"encoding/json"
	"os"
	"path/filepath"
)

// Config son las reglas ajustables por repo.
type Config struct {
	// Palabras que marcan un commit como "fix" (case-insensitive, multiidioma).
	FixKeywords []string `json:"fix_keywords"`
	// Regex para extraer ids de ticket (grupo 1 = id, o match completo).
	TicketPatterns []string `json:"ticket_patterns"`
	// Globs de ficheros a excluir de hotspots/churn (ruido generado).
	ExcludeGlobs []string `json:"exclude_globs"`
	// Labels de PR/issue que marcan un bug (señal determinista del forge).
	BugLabels []string `json:"bug_labels"`
	// Taxonomía de categorías de bug: categoría -> palabras clave que la marcan.
	// Un commit fix se clasifica por coincidencia de subcadena (case-insensitive).
	BugCategories map[string][]string `json:"bug_categories"`
}

// Default son reglas sensatas de arranque. El usuario las afina por repo.
func Default() Config {
	return Config{
		FixKeywords: []string{
			"fix", "bug", "hotfix", "patch", "fixes", "fixed", "broken",
			"arregl", "corrig", "solucion", "repara", "error", "falla", "roto",
		},
		TicketPatterns: []string{
			`\b([A-Z][A-Z0-9]+-\d+)\b`, // Jira: PROJ-123, BUG-047
			`(#\d+)`,                   // GitHub: #123, Closes #123
		},
		ExcludeGlobs: []string{
			"*-lock.json", "*.lock", "package-lock.json", "*.min.js", "*.csv",
			"dist/*", "build/*", "vendor/*", "node_modules/*",
		},
		BugLabels: []string{"bug", "defect", "regression", "error", "fix", "hotfix"},
		BugCategories: map[string][]string{
			"memory-safety": {"memory leak", "leak", "use-after-free", "double-free", "segfault", "oom", "out of memory", "uninitialized", "buffer overflow"},
			"crash":         {"crash", "panic", "abort", "assertion", "sigsegv", "sigabrt", "fatal"},
			"concurrency":   {"race condition", "data race", "deadlock", "mutex", "thread safety", "atomic"},
			"network":       {"tls", "ssl", "socket", "websocket", "http/2", "http2", "tcp", "keep-alive", "handshake", "timeout", "fetch"},
			"install-deps":  {"lockfile", "bun.lock", "dependency", "dependencies", "registry", "workspace", "node_modules", "npm install"},
			"types":         {"typescript", "d.ts", "tsconfig", "type definition", "typings"},
			"parser":        {"parser", "lexer", "transpiler", "syntax error", "tokenizer", "ast "},
			"performance":   {"performance", "regression", "memory usage", "cpu usage", "slow"},
			"security":      {"security", "vulnerability", "cve", "injection", "sanitize"},
			"data-db":       {"sqlite", "postgres", "mysql", "serialize", "deserialize", "encoding", "sql query"},
			"windows":       {"windows", "win32"},
			"build":         {"bundler", "compile error", "linker", "codegen", "cross-compile"},
		},
	}
}

// Load lee <repoRoot>/.chrono/config.json; si falta o es inválido, usa Default.
func Load(repoRoot string) Config {
	p := filepath.Join(repoRoot, ".chrono", "config.json")
	b, err := os.ReadFile(p)
	if err != nil {
		return Default()
	}
	c := Default()
	if json.Unmarshal(b, &c) != nil {
		return Default()
	}
	return c
}

// WriteDefault escribe la config por defecto si aún no existe.
func WriteDefault(chronoDir string) error {
	p := filepath.Join(chronoDir, "config.json")
	if _, err := os.Stat(p); err == nil {
		return nil // no pisar la del usuario.
	}
	b, _ := json.MarshalIndent(Default(), "", "  ")
	return os.WriteFile(p, b, 0o644)
}
