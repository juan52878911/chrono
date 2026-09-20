// Package domain define el modelo de conocimiento y las métricas de chrono.
//
// REGLA HEXAGONAL: este paquete NO hace I/O y NO conoce git, SQLite ni HTTP.
// Habla en términos de dominio (Revision, Author, Change), de modo que una
// fuente que no sea git podría, en teoría, poblarlo.
package domain

// ChangeType clasifica qué le pasó a un fichero en un commit.
type ChangeType string

const (
	Modified ChangeType = "M"
	Renamed  ChangeType = "R" // OldPath queda poblado.
	Binary   ChangeType = "B"
)

// FileChange es una línea de `git log --numstat` ya interpretada.
type FileChange struct {
	Path     string
	OldPath  string // solo en Renamed.
	Type     ChangeType
	Added    int // -1 si binario (numstat da "-").
	Deleted  int // -1 si binario.
	IsBinary bool
}

// Revision es un commit normalizado. Se llama "Revision" a propósito, no
// "Commit": el núcleo no asume git (ver DECISIONS.md). El autor llega crudo
// (ya resuelto por mailmap) y el adaptador de almacén lo unifica a un id.
type Revision struct {
	SHA         string
	AuthorName  string
	AuthorEmail string
	When        string // ISO-8601 (%aI), comparable lexicográficamente.
	Subject     string
	Body        string
	Simhash     uint64 // huella para agrupar casi-duplicados.
	IsBulk      bool   // toca más de N ficheros -> excluido del acoplamiento.
	Changes     []FileChange
}

// Classification es la salida del puerto Classifier (Nivel 0 en v1).
type Classification struct {
	IsFix      bool
	IsRevert   bool
	Kind       string  // fix|feat|refactor|docs|chore|test|other
	Severity   int     // 1..5, 0 = sin señal
	Confidence float64 // 0..1; en Nivel 0 es heurística.
	Source     string  // "rules" | "local-model" | "jev" | "llm"
}

// --- Resultados de métricas (se serializan según OUTPUT-CONTRACT.md) ---

type Hotspot struct {
	Path      string  `json:"path"`
	Changes   int     `json:"changes"`
	SizeLines int     `json:"size_lines"`
	Score     float64 `json:"score"`
}

type CouplingPair struct {
	B          string  `json:"b"`
	Support    int     `json:"support"`
	Confidence float64 `json:"confidence"`
}

type Ownership struct {
	Name    string  `json:"name"`
	Changes int     `json:"changes"`
	Share   float64 `json:"share"`
}
