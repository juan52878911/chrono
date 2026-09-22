-- Esquema v2 de chrono (port Rust). Convención: `id` (no sha), `entity` (no path).
-- Contrato COMPARTIDO entre chrono-store (escribe) y chrono-metrics (lee).
-- Un evento genérico; un commit git es una proyección (kind='commit').
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Fuentes ingeridas (multi-fuente en un mismo índice).
CREATE TABLE IF NOT EXISTS sources (
    id           TEXT PRIMARY KEY,        -- "git:<abs>" | "jsonl:<abs>" ...
    kind         TEXT NOT NULL,           -- "git" | "jsonl" | ...
    path         TEXT,
    watermark    TEXT,                    -- opaco: "sha:<HEAD>" | "off:<n>|hash"
    manifest_json TEXT,
    last_sync_at TEXT
);

-- Actores: autor, host, servicio... `key` unifica identidades (email, host).
CREATE TABLE IF NOT EXISTS actors (
    id           INTEGER PRIMARY KEY,
    key          TEXT NOT NULL UNIQUE,
    display_name TEXT
);
CREATE TABLE IF NOT EXISTS actor_identities (
    alias    TEXT PRIMARY KEY,            -- email/alias visto
    name     TEXT,
    actor_id INTEGER NOT NULL REFERENCES actors(id)
);

-- Entidades afectadas: fichero, endpoint, host... `key` es jerárquica ("a/b/c"
-- o "a/b/c#symbol"). `type` distingue file|symbol|endpoint|... `size` pondera.
CREATE TABLE IF NOT EXISTS entities (
    id       INTEGER PRIMARY KEY,
    key      TEXT NOT NULL UNIQUE,
    type     TEXT NOT NULL DEFAULT 'file',
    size     INTEGER NOT NULL DEFAULT 0,  -- LOC de HEAD (git) para hotspots; 0 si no
    deleted  INTEGER NOT NULL DEFAULT 0,  -- 1 si ya no está en HEAD
    excluded INTEGER NOT NULL DEFAULT 0   -- 1 si un exclude_glob lo marca (ruido)
);

-- Eventos (era `commits`).
CREATE TABLE IF NOT EXISTS events (
    id        TEXT PRIMARY KEY,           -- sha | hash(línea) | uuid
    source_id TEXT NOT NULL,
    kind      TEXT NOT NULL,              -- "commit" | "log" | ...
    at        TEXT NOT NULL,              -- ISO-8601 UTC para la salida
    at_epoch  INTEGER NOT NULL,           -- segundos UTC; lo que usan ventanas/índices
    actor_id  INTEGER REFERENCES actors(id),
    title     TEXT NOT NULL DEFAULT '',
    body      TEXT NOT NULL DEFAULT '',
    level     TEXT NOT NULL DEFAULT '',   -- severidad nativa si existe
    simhash   INTEGER NOT NULL DEFAULT 0,
    is_bulk   INTEGER NOT NULL DEFAULT 0,
    touches_n INTEGER NOT NULL DEFAULT 0,
    attrs     TEXT NOT NULL DEFAULT '{}'  -- JSON de atributos planos
);

-- Cambios: evento × entidad (era `changes`).
CREATE TABLE IF NOT EXISTS touches (
    event_id  TEXT NOT NULL REFERENCES events(id),
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    weight    INTEGER NOT NULL DEFAULT 0,  -- added+deleted en git; -1 si binario
    attrs     TEXT NOT NULL DEFAULT '{}',  -- git: added, deleted, change_type, old_path
    PRIMARY KEY (event_id, entity_id)
);

-- Relaciones deterministas (absorbe commit_tickets + reverts).
CREATE TABLE IF NOT EXISTS links (
    event_id TEXT NOT NULL REFERENCES events(id),
    rel      TEXT NOT NULL,               -- "ticket" | "reverts" | "pr" | ...
    target   TEXT NOT NULL,
    PRIMARY KEY (event_id, rel, target)
);

-- Etiquetas de clasificación multitarea (generaliza classifications).
CREATE TABLE IF NOT EXISTS labels (
    event_id   TEXT NOT NULL REFERENCES events(id),
    task       TEXT NOT NULL,             -- "is_fix" | "kind" | "bug_category" | ...
    label      TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 0,
    source     TEXT NOT NULL DEFAULT '',  -- "rules" | "jev" | "forge"
    evidence   TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (event_id, task)
);

-- Marcadores (tags/releases/deploys). Era `tags`, generalizado.
CREATE TABLE IF NOT EXISTS markers (
    source_id TEXT NOT NULL,
    name      TEXT NOT NULL,
    at        TEXT,
    ref       TEXT,
    kind      TEXT NOT NULL DEFAULT 'tag',
    PRIMARY KEY (source_id, name)
);

-- Caché de líneas por OID de blob (git size_lines): sync solo lee blobs nuevos.
CREATE TABLE IF NOT EXISTS blob_lines (
    oid   TEXT PRIMARY KEY,
    lines INTEGER NOT NULL
);

-- Índices (mismos accesos que las consultas de metrics).
CREATE INDEX IF NOT EXISTS idx_events_at     ON events(at_epoch);
CREATE INDEX IF NOT EXISTS idx_events_src_at ON events(source_id, at_epoch);
CREATE INDEX IF NOT EXISTS idx_events_kind   ON events(kind, at_epoch);
CREATE INDEX IF NOT EXISTS idx_touches_ent   ON touches(entity_id, event_id);
CREATE INDEX IF NOT EXISTS idx_labels_task   ON labels(task, label);
CREATE INDEX IF NOT EXISTS idx_links_rel     ON links(rel, target);

-- FTS5 externo sobre events (title+body); sin duplicar contenido.
CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
    title, body, content='events', content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
