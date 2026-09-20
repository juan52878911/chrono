-- Esquema del índice chrono (v1, relacional). Vectores (sqlite-vec) en v2.
-- Todo en un único archivo .db portable. Un solo writer; lectura concurrente vía WAL.

-- meta: el manifiesto. Clave/valor. Guarda las decisiones deterministas para
-- que dos máquinas produzcan el mismo índice y para saber cuándo reindexar.
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
-- Claves esperadas: schema_version, git_version, chrono_version,
-- determinism (first_parent, no_merges, rename_threshold), mailmap_used,
-- bulk_threshold, embedding_model_hash (null en v1), last_sha (marca de agua).

-- Identidades: varios (email,nombre) que son el mismo humano -> un author.
CREATE TABLE IF NOT EXISTS authors (
    id            INTEGER PRIMARY KEY,
    canonical_email TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS author_identities (
    email     TEXT PRIMARY KEY,   -- alias observado
    name      TEXT,
    author_id INTEGER NOT NULL REFERENCES authors(id)
);

-- Ficheros (ruta actual). Los renames actualizan path y dejan rastro en changes.
CREATE TABLE IF NOT EXISTS files (
    id        INTEGER PRIMARY KEY,
    path      TEXT NOT NULL UNIQUE,
    is_binary INTEGER NOT NULL DEFAULT 0,
    deleted   INTEGER NOT NULL DEFAULT 0,  -- 1 si ya no existe en HEAD.
    excluded  INTEGER NOT NULL DEFAULT 0,  -- 1 si un exclude_glob lo marca (ruido).
    size_lines INTEGER NOT NULL DEFAULT 0  -- LOC de HEAD, para hotspots.
);

-- Commits (revisiones). is_bulk excluye del acoplamiento.
CREATE TABLE IF NOT EXISTS commits (
    sha        TEXT PRIMARY KEY,
    author_id  INTEGER NOT NULL REFERENCES authors(id),
    authored_at TEXT NOT NULL,   -- ISO-8601, para ventanas --since.
    subject    TEXT NOT NULL,
    body       TEXT,
    simhash    INTEGER NOT NULL DEFAULT 0,  -- huella de casi-duplicados.
    is_bulk    INTEGER NOT NULL DEFAULT 0,
    files_touched INTEGER NOT NULL DEFAULT 0
);

-- Caché de líneas por blob (OID). El sync solo lee blobs nuevos -> rapidísimo.
CREATE TABLE IF NOT EXISTS blob_lines (
    oid   TEXT PRIMARY KEY,
    lines INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_commits_when ON commits(authored_at);
CREATE INDEX IF NOT EXISTS idx_commits_author ON commits(author_id);

-- Cambios por (commit, fichero). El grafo autor-commit-fichero sale de aquí.
CREATE TABLE IF NOT EXISTS changes (
    commit_sha TEXT NOT NULL REFERENCES commits(sha),
    file_id    INTEGER NOT NULL REFERENCES files(id),
    change_type TEXT NOT NULL,   -- A|M|D|R
    old_path   TEXT,             -- solo en rename
    added      INTEGER NOT NULL DEFAULT 0,  -- -1 si binario
    deleted    INTEGER NOT NULL DEFAULT 0,  -- -1 si binario
    PRIMARY KEY (commit_sha, file_id)
);
CREATE INDEX IF NOT EXISTS idx_changes_file ON changes(file_id);

-- Clasificación (Nivel 0 en v1: reglas). Una fila por commit.
CREATE TABLE IF NOT EXISTS classifications (
    commit_sha TEXT PRIMARY KEY REFERENCES commits(sha),
    kind       TEXT NOT NULL,     -- fix|feat|refactor|docs|chore|test|other
    is_fix     INTEGER NOT NULL DEFAULT 0,
    is_revert  INTEGER NOT NULL DEFAULT 0,
    severity   INTEGER NOT NULL DEFAULT 0,  -- 1..5, 0 sin señal
    confidence REAL NOT NULL DEFAULT 0,
    source     TEXT NOT NULL DEFAULT 'rules'
);
CREATE INDEX IF NOT EXISTS idx_class_fix ON classifications(is_fix);

-- Tickets mencionados en mensajes/PRs (regex: PROJ-123, #123, Closes/Fixes).
CREATE TABLE IF NOT EXISTS tickets (
    id       TEXT PRIMARY KEY,    -- normalizado, p.ej. "PROJ-123" o "#123"
    tracker  TEXT                 -- jira|github|... (null si desconocido)
);
CREATE TABLE IF NOT EXISTS commit_tickets (
    commit_sha TEXT NOT NULL REFERENCES commits(sha),
    ticket_id  TEXT NOT NULL REFERENCES tickets(id),
    PRIMARY KEY (commit_sha, ticket_id)
);

-- PRs e issues del forge (puerto Tracker). is_bug se deriva de las labels.
CREATE TABLE IF NOT EXISTS issues (
    number    INTEGER PRIMARY KEY,   -- número compartido PR/issue en GitHub
    kind      TEXT NOT NULL,         -- pr | issue
    title     TEXT,
    state     TEXT,                  -- OPEN | CLOSED | MERGED
    labels    TEXT,                  -- coma-separadas
    is_bug    INTEGER NOT NULL DEFAULT 0,
    merged    INTEGER NOT NULL DEFAULT 0,
    author    TEXT,
    closed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_issues_bug ON issues(is_bug);

-- Enlace determinista PR -> commit (de los oids que devuelve el forge).
CREATE TABLE IF NOT EXISTS pr_commits (
    number     INTEGER NOT NULL,
    commit_sha TEXT NOT NULL,
    PRIMARY KEY (number, commit_sha)
);
CREATE INDEX IF NOT EXISTS idx_prc_commit ON pr_commits(commit_sha);

-- Etiquetas de git = fases del proyecto.
CREATE TABLE IF NOT EXISTS tags (
    name      TEXT PRIMARY KEY,
    tagged_at TEXT,
    sha       TEXT
);

-- Acoplamiento temporal materializado (recalculado en sync). Excluye is_bulk.
-- Se guarda podado por min-support para no explotar en disco.
CREATE TABLE IF NOT EXISTS coupling (
    file_a     INTEGER NOT NULL REFERENCES files(id),
    file_b     INTEGER NOT NULL REFERENCES files(id),
    support    INTEGER NOT NULL,   -- commits en que cambiaron juntos
    confidence REAL NOT NULL,      -- support / cambios_de_a
    PRIMARY KEY (file_a, file_b)
);
