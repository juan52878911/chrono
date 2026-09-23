# Diseño — núcleo general, JEV embebido y migración a Rust

> Estado: **diseño aprobado, en desarrollo**. Documento vivo.
> Decisiones del dueño tomadas (2026-09): ver §0.

Objetivo: que chrono deje de ser "solo git" y analice **cualquier traza histórica**
(logs, changelogs, timelines de incidentes, CSV con tiempo, despliegues…),
con un clasificador propio embebido (**JEV**), manteniendo lo que ya funciona:
binario único, local, determinista, sin servidor, sin runtime externo, MIT.

---

## 0. Decisiones tomadas

1. **Contrato de salida**: se renombra a `entity`/`id` (en vez de mantener `path`/`sha`
   como alias). Es más limpio y rompe compatibilidad de forma consciente → `schema_version: 2`.
2. **Lenguaje**: se **migra todo a Rust** *antes* de la generalización, por rendimiento
   y control de memoria. El binario Go actual (v0.1.1) se mantiene funcionando hasta que
   el port Rust alcance paridad; conviven en el repo (`/` = Go actual, `rust/` = port).
3. **Clasificador por defecto**: `rules` (Level-0) hasta que exista `docs/JEV-EVAL.md`
   con métricas que demuestren mejora. JEV entra **opt-in** (`classifier: rules+jev`).
4. **Migración de índice v1→v2**: **reindex** avisado (el índice es regenerable en segundos;
   no se hace migración in-place).

> Nota honesta sobre Rust: el binario Go ya responde consultas en <0,5 s y pesa ~7 MB.
> Rust aporta sobre todo (a) ingesta/featurización paralela con memoria controlada para
> trazas de millones de líneas, (b) inferencia JEV determinista sin las trampas de FMA de Go,
> (c) un solo lenguaje para trainer y serving. El coste es una reescritura completa; por eso
> se hace por fases con paridad verificada contra el Go actual en cada paso.

---

## 1. Diagnóstico del estado actual (Go v0.1.1)

Lo que **ya sirve** y se porta tal cual (conceptualmente):

- Ingesta en streaming con cursor sobre un flujo y RAM constante (`internal/ingest/gitlog.go`).
- Watermark + detección de divergencia → reindex (`Sync`).
- Un writer transaccional con cachés en memoria (`internal/store/store.go`).
- Manifiesto en `meta`, `schema_version`, envelope con `token_budget`/`truncated`.
- SimHash (8 B/evento), FTS5 opcional con fallback LIKE.
- Clasificación configurable con `Source` y `Confidence` en el resultado.
- Señales deterministas fuertes: revert git-nativo, conventional commits, labels del forge.
- Consultas relacionales sin nada git dentro del SQL (hotspots/coupling/owners/churn).

**Obstáculos** que se corrigen en la migración:

1. Los puertos `Source`/`Classifier` están documentados pero **no existen como abstracción**:
   hoy `ingest.Run` shell-ea git directamente. En Rust nacen como **traits** reales.
2. `domain.Revision` tiene forma de git (`sha`, `changes`, `%aI`). Se sustituye por `Event`.
3. **Timestamps como texto con offset local** comparados lexicográficamente: bug de orden
   real hoy en los bordes, peor al mezclar fuentes. → normalizar a **UTC + epoch entero**.
4. `store.Migrate` no es migración; `schema_version` se escribe pero no se compara. → detección real.
5. FTS se reconstruye entera en cada `sync` y duplica el texto. → **FTS externa incremental**.
6. `Similar` escanea todas las filas en Go. → **bandas SimHash** cuando >1M eventos.
7. Deuda a sanear: tabla `coupling` muerta (nunca se usa), `Severity` y `tickets.tracker`
   nunca se rellenan, `Search` pasa texto crudo a `MATCH` (un `:` o comillas lo rompen).

---

## 2. Núcleo general — modelo `Event`

`Event` es el registro temporal abstracto; un commit git es **una proyección**.
Convención decidida: **`id`** (no `sha`) y **`entity`** (no `path`).

```rust
pub struct Event {
    pub id: String,                     // único en la fuente: sha | hash(línea+offset) | uuid
    pub source_id: String,              // "git:<abs>" | "jsonl:<abs>" | "journald:<unit>"
    pub kind: String,                   // "commit" | "log" | "deploy" | "incident" | "release" | "row"
    pub at: String,                     // ISO-8601 SIEMPRE UTC ("2026-08-30T10:00:00Z")
    pub at_epoch: i64,                  // segundos UTC; lo que usan índices y ventanas
    pub actor: Actor,                   // quién/qué (autor, host, servicio, usuario)
    pub title: String,                  // subject | línea normalizada | título del incidente
    pub body: String,                   // cuerpo | payload (truncado a cap)
    pub level: String,                  // severidad NATIVA de la fuente si existe; "" si no
    pub attrs: BTreeMap<String,String>, // atributos planos del adaptador (status=500, method=GET…)
    pub touches: Vec<Touch>,            // entidades afectadas
    pub links: Vec<Link>,               // relaciones deterministas con otros ids
    pub simhash: u64,
    pub is_bulk: bool,                  // > umbral de touches → fuera de coupling
}

pub struct Actor { pub name: String, pub key: String }   // key = email | host | service (unifica identidades)

pub struct Touch {
    pub entity: String,                 // clave JERÁRQUICA con "/" ("src/bun.js/socket.zig", "api/users/{id}")
    pub entity_type: String,            // "file" | "endpoint" | "host" | "service" | "table"
    pub weight: i64,                    // magnitud: added+deleted en git; 1 en logs
    pub attrs: BTreeMap<String,String>, // git: added, deleted, change_type, old_path, binary
}

pub struct Link { pub rel: String, pub target: String }  // rel: "ticket"|"reverts"|"parent"|"deploy-of"|"pr"
```

**Clave del diseño**: `Touch.entity` como ruta jerárquica separada por `/`. Así `hotspots`,
`coupling`, `owners` y `churn` (prefijo `LIKE`, `topDir`) funcionan igual para código,
endpoints o hosts. Cada adaptador **debe** emitir claves jerárquicas con sentido.

**Mapeo git → Event**: `sha→id (kind="commit")`, autor(mailmap)→`Actor{key:email}`,
`%aI→at` (a UTC) + `at_epoch`, subject/body→title/body, cada `FileChange`→`Touch`
(`entity=path`, `entity_type="file"`, `weight=|added|+|deleted|`, resto a `attrs`),
revert→`Link{rel:"reverts"}`, ticket→`Link{rel:"ticket"}`.

---

## 3. Puerto Source — traits, cursor, registro

```rust
pub struct Watermark { pub kind: String, pub value: String } // git:"sha:<HEAD>"; file:"off:<bytes>|hash-4k"

pub trait Source {
    fn kind(&self) -> &str;                       // "git"|"jsonl"|"csv"|"syslog"|"nginx"|"journald"|"changelog"
    fn detect(&self, path: &Path) -> i32;         // 0 = no reconocido; >0 = confianza
    fn open(&self, path: &Path, wm: Option<Watermark>, cfg: &SourceConfig) -> Result<Box<dyn Cursor>>;
}

pub trait Cursor {
    fn next(&mut self) -> Result<Option<Event>>;  // streaming, uno a uno
    fn watermark(&self) -> Watermark;
    fn manifest(&self) -> BTreeMap<String,String>;// claves deterministas de ESTA fuente
}
```

- **Registro explícito** (lista, no `init` mágico): determinista y legible. `init <path>`
  recorre los Source, llama a `detect`, elige el de mayor score; `--as <kind>` fuerza.
- **Divergencia generalizada**: watermark que no casa (sha inexistente; fichero más corto
  que el offset; hash de los primeros 4 KB distinto → rotación) → reindex de esa fuente.
- **Multi-fuente en un `.chrono/`**: tabla `sources`. git del repo + `deploys.jsonl` +
  `incidents.csv` correlacionables por tiempo y por `links`. Ahí está el valor real.
- **Adaptadores v1** (todos sin runtime externo): `git`, `jsonl`, `csv`, `textlog`
  (syslog/nginx por presets regex), `changelog` (Keep-a-Changelog → `kind="release"`),
  `journald` (shell-out a `journalctl -o json`, como se hace hoy con `git`/`gh`).
- Configuración de columnas/regex/presets en `.chrono/config.json` (reproducible, versionable).

**Qué consultas generalizan**: hotspots, coupling, owners, churn, search, similar, phases
(→ `markers`), tickets (→ `links`). **Específicas de git** (se quedan en el adaptador):
`branches` (git en vivo), `size_lines`/renames/binarios, reverts git-nativos, forge/`prs`.
**Nuevas** (fase 2): `timeline [--bucket 1h]`, `top <dim>`, `correlate <id>`, `events`.

---

## 4. Esquema SQLite v2 (nombres `entity`/`id`)

```sql
sources   (id TEXT PK, kind, path, watermark, manifest_json, last_sync_at)
actors    (id INTEGER PK, key TEXT UNIQUE, display_name)
actor_identities (alias TEXT PK, name, actor_id)
entities  (id INTEGER PK, key TEXT UNIQUE, type, size INTEGER DEFAULT 0, deleted, excluded)
events    (id TEXT PK, source_id, kind, at TEXT, at_epoch INTEGER, actor_id, title, body,
           level TEXT, simhash INTEGER, is_bulk, touches_n, attrs TEXT /*JSON*/)
touches   (event_id, entity_id, weight INTEGER, attrs TEXT, PK(event_id, entity_id))
event_dims(event_id, key, value)   -- SOLO claves de baja cardinalidad (status, level, unit, host)
links     (event_id, rel, target, PK(event_id, rel, target))
labels    (event_id, task, label, confidence REAL, source, evidence TEXT, PK(event_id, task))
markers   (source_id, name, at, ref, kind)
templates (id INTEGER PK, source_id, template TEXT, simhash)
rollups   (source_id, template_id, bucket_epoch, count, first_id, last_id)
events_fts USING fts5(title, body, content='events', content_rowid=rowid)  -- externo, sin duplicar
issues, pr_commits, blob_lines   -- adaptador git/tracker
```

- `at_epoch INTEGER` indexado para ventanas y buckets; `at` textual para salida.
- `attrs` JSON en columna (SQLite ≥3.45 trae JSON en core). `event_dims` (EAV) solo baja cardinalidad.
- Índices: `events(at_epoch)`, `events(source_id,at_epoch)`, `events(kind,at_epoch)`,
  `touches(entity_id,event_id)` (covering), `event_dims(key,value,event_id)`,
  `labels(task,label)`, `links(rel,target)`.
- La tabla `coupling` desaparece (nunca se usó). `classifications` → `labels` multitarea.
- **Migración**: `open` compara `schema_version`; si <2 → mensaje accionable + `init` reconstruye.
  Consultar un índice v1 falla limpio en vez de dar resultados a medias.

---

## 5. JEV dentro del binario

**Algoritmo**: regresión logística multiclase (una cabeza por tarea) sobre *hashing trick*
(FNV-1a → 2^17/2^18 buckets), con features: uni+bigramas de `title`(+300 chars de `body`),
char n-gramas 3–5, y features estructurales (`src:<kind>`, `lvl:<level>`, `conv:<prefijo>`,
`n_touch:<bucket>`, `top:<1er segmento de cada entity>`, `has_ticket`, `is_revert`,
`attr:status=5xx`…). Descartados: NB (mal calibrado; sirve de baseline), SVM (sin probas),
transformer/ONNX (CGO/tamaño). Opción B si LR se queda corto: fastText cuantizado.

**Determinismo cross-plataforma**: pesos **`i16` cuantizados**, features `i32`, acumulación
`i64` en orden fijo, argmax con desempate por índice; softmax en `f64` solo para reportar.
En Rust esto es directo y sin la trampa del FMA de Go.

**Entrenar fuera / servir dentro**:
- Trainer = crate/bin aparte (`chrono-jev-train`), **no se distribuye**. En Rust para reutilizar
  el **mismo tokenizador** que sirve (evita deriva). SGD/AdaGrad + L1.
- Dataset por **supervisión débil gratis**: conventional commits de repos públicos
  (etiqueta = prefijo, **eliminado del texto**), labels del forge, Loghub para logs.
  `chrono export-training` produce el JSONL desde cualquier índice.
- Artefacto `jev-<task>-v<N>.bin` (magic, feature_spec_hash, buckets, labels, bias, pesos
  sparse `i16` con L1 podado), gzip, embebido con `include_bytes!`. ~**+2–3 MB** por 4 cabezas.
  Override del usuario en `.chrono/models/`. Hash del modelo activo en `meta` y manifiesto;
  cambiarlo dispara **reclasificación** (recalcula `labels` desde `events`, sin reingerir).

**Integración como Level-1** (trait `Classifier` + cadena):
1. Señales duras (revert, conventional, forge, `level` nativo) → `source="rules"`, **ganan siempre**.
2. Si no hay señal dura → JEV; si `p_max ≥ τ_task` → `source="jev"` + `evidence` (top features por `w·x`).
3. Si `p_max < τ` → reglas blandas (`fix_keywords`, conf 0.5) u `other`. No se inventan categorías.

**Honestidad**: `JEV-EVAL.md` por versión; **opt-in** hasta demostrar mejora. Objetivos
realistas: `kind` macro-F1 ~0.75–0.85, `bug_category` ~0.5–0.65. **Severidad 1–5 desde prosa:
no fiable, no se promete.** Solo severidad nativa de la fuente. ES/EN en v1.

**Latencia**: 5–20 µs/evento → <0,5 s extra en 17.678 commits; en logs se clasifica una vez
por template y se hereda (segundos, no minutos).

---

## 6. Optimización a escala

- **Pipeline determinista con paralelismo** (aquí gana Rust): `Cursor.next` asigna `seq` →
  N workers de featurización+SimHash+JEV → writer que inserta **en orden de `seq`**
  (IDs/rowids deterministas pese al paralelismo). Test: ingerir dos veces y comparar
  `sha256` del `.db` tras `VACUUM`.
- **Sentencias preparadas** en el writer (una vez, no SQL literal por fila). Medir con
  `--profile` cuánto es lectura de la fuente vs SQLite antes de optimizar.
- **FTS externa incremental** (`content='events'`): solo eventos nuevos en `sync`, sin
  duplicar texto (−30/40 % de `.db`), `'optimize'` al final.
- **Templates + rollups** (clave para logs): normalización Drain-light (números, hex, UUID,
  IPs, rutas→placeholders); `timeline`/`top` responden desde rollups. nginx 10M líneas
  ≈ 2 GB → rollups a 1 min ≈ 50–70 MB, <100 ms.
- **Caps con efecto declarado** en manifiesto: `max_events_per_source` (5M), `body_max_bytes`
  (4 KB), `sample_levels` (muestreo determinista `hash(id) mod N`), `since` en ingesta.
- **Bandas SimHash** solo >1M eventos (4–5 columnas de 16 bits indexadas → `similar` <50 ms).
- Regla de oro: **ninguna consulta escanea `events` completo** — leen índices o agregados.
- `chrono bench` fija los números sobre bun (17.678) y un fixture sintético de 10M líneas.

---

## 7. Plan por fases

Con Rust primero, el orden es: montar el port Rust del núcleo → paridad con git →
adaptadores → JEV → escala. Cada fase verifica paridad contra el Go actual (bun: 17.678 = 17.678).

| Fase | Contenido | Estado |
| --- | --- | --- |
| **R0 · Cimientos Rust** | Workspace Cargo; crate `chrono-core` (dominio `Event`/`Touch`/`Link`/`Label` con `entity`/`id` + traits `Source`/`Cursor`/`Classifier`), sin deps. | **hecho (este commit)** |
| **R1 · Store + adaptador git + paridad** | `chrono-store` (SQLite v2), `chrono-source-git` (ingesta git tras `Cursor`), consultas hotspots/coupling/owners/churn; CLI mínima `init`+`hotspots`. Verificar 17.678=17.678 y top-15 vs Go. | pendiente |
| **R2 · Resto de consultas + MCP + i18n** | bugs(rules)/tickets/prs/branches/search/similar/phases; servidor MCP; i18n EN/ES; contrato v2 `entity`/`id`; paridad total con Go → sustituye al binario Go. | pendiente |
| **R3 · Adaptadores + multi-fuente + rollups** | **hecho:** jsonl, **csv**, **textlog** (syslog/nginx sin `regex`), **changelog** (Keep a Changelog → `release`); **`sources` poblada + `chrono add` + `sync` multi-fuente** (paridad git verificada; divergencia por longitud+hash de prefijo); `SourceConfig.options` desde `.chrono/config.json`; consultas log-native **`timeline`/`top`/`correlate`** (leen eventos crudos) en metrics + CLI + MCP. **pendiente:** journald (necesita Linux); templates+rollups+`event_dims` (logs de millones de líneas, oleada propia con eval); `events`. | casi completo |
| **R4 · JEV Level-1** | crate `chrono-jev` (features+inferencia i16), `chrono-jev-train`, dataset, `JEV-EVAL.md`, opt-in. | pendiente |
| **R5 · Escala medida** | bandas SimHash; `bench`; caps reales; paginación `gh` (cap 1000 pendiente). | pendiente |

---

## 8. Riesgos, anti-scope y decisiones abiertas

**Riesgos**: perder nitidez git al generalizar (mitigado con `Touch.attrs`+`entities.size` y
test de paridad); adaptadores de texto sin preset (→ `detect` **rechaza**, no ingiere basura);
JEV sin eval (→ `JEV-EVAL.md` + opt-in, como ya se documentó el fallo del clustering por
frecuencia en `DECISIONS.md §5`); determinismo con paralelismo (→ `seq`+writer ordenado+i16);
**coste de la reescritura Rust** (→ paridad verificada por fase; el Go sigue vivo hasta R2).

**Anti-scope**: journal binario/PCAP/OTel-proto/Kafka; detección de anomalías/causa raíz/
predicción; embeddings/LLM en el binario (fase B, proveedor externo); JEV sin dataset+métrica;
`serve`/multi-repo remoto (segundo producto).

**Decisiones abiertas** (menores, para más adelante): caps por defecto a validar con un log
real; definición de "problema" en fuentes no git (`level`/`status` por config); override de
modelos en `.chrono/models/` (recomendado sí).

---

## 9. Historia de símbolos (barata) — decisión 2026-09-20

Revoca `DECISIONS.md §4` a "**símbolos baratos sí, AST no**". Ruta elegida: **S0+S1**
(sin tree-sitter). El coste real es tiempo de `init`, no tamaño de binario → va **opt-in**.

**Distinción clave**:
- **Ver el diff** ≠ **entender el símbolo**. Ver es gratis (shell-out); el símbolo exacto por
  AST es carísimo en ingesta (parsear cada blob antes/después de cada commit: bun = minutos, GB).
- El **nombre de la función del hunk** es gratis: git ya emite `@@ -a,b +c,d @@ <funcname>`.

**Modelo** (sin tocar el struct): un símbolo es **otro `Touch`**.
```
Touch { entity: "src/bun.js/socket.zig#connect", entity_type: "symbol",
        weight: added+deleted de sus hunks,
        attrs: { file, sym_kind: fn|type|test, hunks, origin: funcname|decl } }
```
Separador **`#`** (no `/`): así el `LIKE 'dir/%'` de owners/hotspots **no** cuenta doble el
touch de fichero y el de símbolo. El fichero sigue siendo su propio `Touch` (`type="file"`);
`is_bulk` se calcula solo sobre ficheros. `entities.type` (ya en v2) filtra: las consultas
llevan `WHERE type='file'` por defecto y `--by symbol` cambia el filtro → **consultas nuevas
gratis**: `hotspots --by symbol`, `coupling "socket.zig#connect"`, `owners "socket.zig#"`.

**Determinismo del funcname**: no fiarse de los drivers `xfuncname` builtin de la versión de
git del usuario; chrono inyecta **sus propias regex por lenguaje** vía
`-c core.attributesFile=.chrono/attributes -c diff.<lang>.xfuncname='…'` y **hashea esas
regex en el manifiesto** (`symbols_rules_hash`). Cambio de regex → reindex de touches símbolo.

**Fases**:

| Fase | Contenido | Binario | Cuándo |
| --- | --- | --- | --- |
| **S0 · Ver diffs** | **hecho.** `chrono show <id> [--entity p]` → `git show` acotado (60 KB, recorte en frontera de línea). NO indexa (respeta §2). | +0 | hecho |
| **S1 · Símbolos por hunk** | **hecho.** `init --symbols` (default off): paso de enriquecimiento APARTE (no toca el cursor de git → paridad intacta) que streamea `git log --no-merges -U0 -p` con regex `xfuncname` PROPIAS por lenguaje (rust/go/python/js-ts/c-cpp/zig/java) inyectadas vía `-c core.attributesFile`/`-c diff.<lang>.xfuncname`, y emite `Touch{type:"symbol", key:"file#func"}` keyeado por sha. `--by symbol` en hotspots/coupling/owners/churn (excluyen símbolos por defecto). Solo ficheros con extensión de código conocida (evita el ruido del xfuncname por defecto de git en `.lock`/`.yml`). `sync` recomputa el delta si el índice tiene símbolos. `symbols_rules_hash` en `meta`. Aproximado a propósito (S3 si midiéramos >15-20% mal). | +~0 MB | hecho |
| **S2 · Tamaño de símbolos** | regex sobre los blobs de HEAD que ya se leen → `entities.size` de símbolos → hotspots ponderados. Sin S2, `hotspots --by symbol` rankea por frecuencia (funciona). | +0 | pendiente |
| **S3 · tree-sitter** (condicional, NO por defecto) | feature Cargo `ast`, runtime+rust+go+python+c (~3,7 MB), **solo si S1 mide >15–20 % de hunks mal atribuidos**. Build oficial sin la feature. | +3,7 MB solo en esa build | tras R4 |

**Anti-scope** (además del §8): no embeber 40 gramáticas ni TS/C++; no `.so`/`.wasm` ni
descargas; no parsear la historia por defecto; no call graph / renames de función /
complejidad / LSP; no indexar diffs; nombres de símbolo no son features de JEV.

