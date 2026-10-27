-- Esquema v6 de chrono (port Rust; ver SCHEMA_VERSION en lib.rs). Convención: `id` (no sha), `entity` (no path).
-- Contrato COMPARTIDO entre chrono-store (escribe) y chrono-metrics (lee).
-- Un evento genérico; un commit git es una proyección (kind='commit').
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Fuentes ingeridas (multi-fuente en un mismo índice).
-- `ref` es alias explícito del rowid (INTEGER PRIMARY KEY): estable ante VACUUM
-- —al revés que un rowid implícito, que VACUUM puede renumerar y dejaría
-- huérfanos los `events.source_ref`. `id` (el string "git:<abs>") pasa de PK a
-- UNIQUE para conservar el `ON CONFLICT(id)` de `upsert_source`. Ver v5 (§DECISIONS).
CREATE TABLE IF NOT EXISTS sources (
    ref          INTEGER PRIMARY KEY,     -- entero estable; lo referencia events.source_ref
    id           TEXT NOT NULL UNIQUE,    -- "git:<abs>" | "jsonl:<abs>" ...
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
-- v6: `rid` es alias explícito del rowid (estable ante VACUUM, como
-- `sources.ref`) y la clave que usan touches/labels/links/event_dims; `id`
-- pasa a UNIQUE y sigue siendo lo que enseñan las salidas. `at` ya no se guarda:
-- es una columna virtual derivada de `at_epoch` (mismo formato que escribían
-- los adaptadores: milisegundos solo si no son 0; '' sin tiempo).
CREATE TABLE IF NOT EXISTS events (
    rid        INTEGER PRIMARY KEY,
    id         TEXT NOT NULL UNIQUE,      -- sha | hash(línea) | uuid
    source_ref INTEGER NOT NULL REFERENCES sources(ref),  -- entero (v5); texto por JOIN
    kind      TEXT NOT NULL,              -- "commit" | "log" | ...
    at_epoch  INTEGER NOT NULL,           -- MILISEGUNDOS UTC; lo que usan ventanas/índices
    actor_id  INTEGER REFERENCES actors(id),
    title     TEXT NOT NULL DEFAULT '',
    body      TEXT NOT NULL DEFAULT '',
    level     TEXT NOT NULL DEFAULT '',   -- severidad nativa si existe
    simhash   INTEGER NOT NULL DEFAULT 0,
    is_bulk   INTEGER NOT NULL DEFAULT 0,
    touches_n INTEGER NOT NULL DEFAULT 0,
    attrs     TEXT NOT NULL DEFAULT '{}', -- JSON de atributos planos (sin las dims)
    at        TEXT GENERATED ALWAYS AS (
        CASE WHEN at_epoch = 0 THEN '' ELSE
            strftime('%Y-%m-%dT%H:%M:%S', (at_epoch - ((at_epoch % 1000 + 1000) % 1000)) / 1000, 'unixepoch')
            || CASE WHEN at_epoch % 1000 = 0 THEN '' ELSE '.' || printf('%03d', (at_epoch % 1000 + 1000) % 1000) END
            || 'Z'
        END) VIRTUAL                      -- ISO-8601 UTC para la salida
);

-- Cambios: evento × entidad (era `changes`).
CREATE TABLE IF NOT EXISTS touches (
    event_rid INTEGER NOT NULL REFERENCES events(rid),
    entity_id INTEGER NOT NULL REFERENCES entities(id),
    weight    INTEGER NOT NULL DEFAULT 0,  -- added+deleted en git; -1 si binario
    added     INTEGER NOT NULL DEFAULT 0,  -- v5: líneas añadidas (antes json_extract en churn)
    deleted   INTEGER NOT NULL DEFAULT 0,  -- v5: líneas borradas (git binario = -1, clamp en churn)
    attrs     TEXT NOT NULL DEFAULT '{}',  -- git: added, deleted, change_type, old_path
    PRIMARY KEY (event_rid, entity_id)
) WITHOUT ROWID;

-- Relaciones deterministas (absorbe commit_tickets + reverts). v7: el par
-- (rel, target) se guarda una vez en `link_targets` y `event_links` lo une al
-- evento por entero; la vista `links` conserva la forma de antes para leer.
CREATE TABLE IF NOT EXISTS link_targets (
    id     INTEGER PRIMARY KEY,
    rel    TEXT NOT NULL,                 -- "ticket" | "reverts" | "pr" | ...
    target TEXT NOT NULL,
    UNIQUE (rel, target)
);
CREATE TABLE IF NOT EXISTS event_links (
    event_rid INTEGER NOT NULL REFERENCES events(rid),
    target_id INTEGER NOT NULL REFERENCES link_targets(id),
    PRIMARY KEY (event_rid, target_id)
) WITHOUT ROWID;
CREATE VIEW IF NOT EXISTS links AS
    SELECT l.event_rid AS event_rid, t.rel AS rel, t.target AS target
    FROM event_links l JOIN link_targets t ON t.id = l.target_id;

-- Etiquetas de clasificación multitarea (generaliza classifications).
CREATE TABLE IF NOT EXISTS labels (
    event_rid  INTEGER NOT NULL REFERENCES events(rid),
    task       TEXT NOT NULL,             -- "is_fix" | "kind" | "bug_category" | ...
    label      TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 0,
    source     TEXT NOT NULL DEFAULT '',  -- "rules" | "jev" | "forge"
    evidence   TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (event_rid, task)
) WITHOUT ROWID;

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

-- PRs e issues del forge (GitHub vía gh). Era `issues` en el Go.
CREATE TABLE IF NOT EXISTS issues (
    number    INTEGER NOT NULL,
    kind      TEXT NOT NULL,              -- "pr" | "issue"
    title     TEXT NOT NULL DEFAULT '',
    state     TEXT NOT NULL DEFAULT '',   -- OPEN | CLOSED | MERGED
    labels    TEXT NOT NULL DEFAULT '',   -- CSV de labels
    merged    INTEGER NOT NULL DEFAULT 0,
    closed_at TEXT NOT NULL DEFAULT '',
    is_bug    INTEGER NOT NULL DEFAULT 0, -- 1 si algún label lo marca como bug
    PRIMARY KEY (kind, number)
);

-- Índices (mismos accesos que las consultas de metrics).
CREATE INDEX IF NOT EXISTS idx_events_at     ON events(at_epoch);
CREATE INDEX IF NOT EXISTS idx_events_srcref_at ON events(source_ref, at_epoch);
CREATE INDEX IF NOT EXISTS idx_touches_ent   ON touches(entity_id, event_rid);
CREATE INDEX IF NOT EXISTS idx_labels_task   ON labels(task, label);
CREATE INDEX IF NOT EXISTS idx_event_links_target ON event_links(target_id, event_rid);
CREATE INDEX IF NOT EXISTS idx_link_targets_target ON link_targets(target);

-- `events --level` y `top level` sin filtros. v6: ya no hay `idx_events_kind`
-- (en logs había un solo kind; los conteos por kind salen de `dim_rollups`).
CREATE INDEX IF NOT EXISTS idx_events_level ON events(level, at_epoch);
-- v6, parcial: los logs no traen actor y pagaban un índice entero de NULLs.
CREATE INDEX IF NOT EXISTS idx_events_actor ON events(actor_id, at_epoch) WHERE actor_id IS NOT NULL;
-- Bandas SimHash para `similar` sin escanear: el simhash de 64 bits se parte en
-- 4 bandas de 16 bits (índices de expresión; `& 65535` toma los 16 bits bajos
-- del desplazamiento, correcto aunque el i64 sea negativo). Dos eventos a
-- distancia Hamming ≤3 comparten al menos una banda idéntica (pigeonhole: 3
-- bits que difieren tocan ≤3 de las 4 bandas), así que la consulta recupera los
-- candidatos por igualdad de banda y refina con Hamming exacto en Rust.
-- v6, parciales: solo commits (`similar` es de commits; los demás eventos se
-- recorren aparte, ver `similar.rs`).
CREATE INDEX IF NOT EXISTS idx_events_sh0 ON events((simhash >> 48) & 65535) WHERE kind = 'commit';
CREATE INDEX IF NOT EXISTS idx_events_sh1 ON events((simhash >> 32) & 65535) WHERE kind = 'commit';
CREATE INDEX IF NOT EXISTS idx_events_sh2 ON events((simhash >> 16) & 65535) WHERE kind = 'commit';
CREATE INDEX IF NOT EXISTS idx_events_sh3 ON events(simhash & 65535) WHERE kind = 'commit';

-- v6 · Dims (P-REL-1): las claves de `attrs` que una fuente declara en `dims`
-- salen del JSON y van aquí, con el valor una sola vez en `dim_values`. Sirven
-- para `top attr:k`, `--attr k:v` y, en la fase 4, para unir fuentes por clave
-- (`dim_id` igual = mismo valor de la misma clave).
CREATE TABLE IF NOT EXISTS dim_values (
    id    INTEGER PRIMARY KEY,
    key   TEXT NOT NULL,
    value TEXT NOT NULL,
    UNIQUE (key, value)
);
CREATE TABLE IF NOT EXISTS event_dims (
    dim_id    INTEGER NOT NULL REFERENCES dim_values(id),
    event_rid INTEGER NOT NULL REFERENCES events(rid),
    PRIMARY KEY (dim_id, event_rid)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_event_dims_ev ON event_dims(event_rid, dim_id);
-- Dims declaradas por fuente (lo que se quitó de `attrs` al ingerir).
CREATE TABLE IF NOT EXISTS source_dims (
    source_ref INTEGER NOT NULL REFERENCES sources(ref),
    key        TEXT NOT NULL,
    PRIMARY KEY (source_ref, key)
) WITHOUT ROWID;
-- Claves que ALGÚN evento de la fuente lleva en `attrs` (superconjunto: no se
-- borran al reingerir). `top attr:k` y `--attr k:v` solo leen el JSON de las
-- fuentes que tienen `k`; sin ninguna, la respuesta es 0 sin recorrer nada.
CREATE TABLE IF NOT EXISTS attr_keys (
    source_ref INTEGER NOT NULL,
    key        TEXT NOT NULL,
    PRIMARY KEY (key, source_ref)
) WITHOUT ROWID;

-- Plantillas Drain-light: un mensaje normalizado (variables → <NUM>/<IP>/…).
-- Miles de líneas de log que solo difieren en valores comparten `template`.
CREATE TABLE IF NOT EXISTS templates (
    id        INTEGER PRIMARY KEY,
    source_id TEXT NOT NULL,
    template  TEXT NOT NULL,
    simhash   INTEGER NOT NULL DEFAULT 0,
    UNIQUE (source_id, template)
);

-- Rollups: conteo de eventos por (fuente, plantilla, bucket temporal). Permiten
-- `patterns`/timeline a escala (millones de líneas) sin escanear `events`.
CREATE TABLE IF NOT EXISTS rollups (
    source_ref   INTEGER NOT NULL,       -- v6: sources.ref (antes el id en texto)
    template_id  INTEGER NOT NULL REFERENCES templates(id),
    bucket_epoch INTEGER NOT NULL,       -- inicio del bucket (múltiplo de ROLLUP_BUCKET_SECS)
    count        INTEGER NOT NULL DEFAULT 0,
    last_rid     INTEGER NOT NULL DEFAULT 0, -- v6: último evento del bucket (events.rid)
    PRIMARY KEY (source_ref, template_id, bucket_epoch)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS idx_rollups_bucket   ON rollups(bucket_epoch);

-- v6 · Rollups por dimensión (P-ESC-1): fuente × plantilla × kind × nivel ×
-- entidad × bucket de 10 min. Responden `top level|kind|entity`, `patterns` y
-- `timeline` con filtros de nivel/kind/entidad/fuente sin recorrer `events`.
-- `level_inf` es el nivel efectivo con `infer_level` (el de la fuente o el
-- deducido del texto). `entity_id`: la entidad del único touch; 0 sin touches;
-- -1 con varios (o un commit, que recibe touches de símbolos después): una
-- consulta por entidad que encuentre -1 en su alcance cae a `events`.
-- `bucket_epoch` = -1 para los eventos sin tiempo (`at_epoch <= 0`).
-- Mismo watermark que `rollups` (`rollup_max_rowid`): se construyen juntos.
CREATE TABLE IF NOT EXISTS dim_rollups (
    source_ref   INTEGER NOT NULL,
    template_id  INTEGER NOT NULL,
    kind         TEXT NOT NULL,
    level        TEXT NOT NULL,
    level_inf    TEXT NOT NULL,
    entity_id    INTEGER NOT NULL,
    bucket_epoch INTEGER NOT NULL,
    count        INTEGER NOT NULL DEFAULT 0,
    last_rid     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (source_ref, template_id, kind, level, level_inf, entity_id, bucket_epoch)
) WITHOUT ROWID;

-- FTS5 externo sobre events (title+body); sin duplicar contenido.
CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
    title, body, content='events', content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);
