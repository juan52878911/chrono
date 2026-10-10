# chrono — benchmarks de escala (P1–P3)

Medido en Apple M4, 16 GB, SSD; binario release (`opt-level="z"`). Fixtures JSONL
sintéticos deterministas. Las cifras "antes" son del análisis de base (agente de
rendimiento sobre un fixture de 1M); las "ahora" se midieron tras cada fase.
Todas las fases preservan **paridad byte-idéntica** y **determinismo** (ver
`DECISIONS.md` §25–26); `CHRONO_NOW` fija el reloj para `.db` reproducible.

## Memoria (RSS pico)

| Momento | RSS pico | Causa / arreglo |
|---|---|---|
| Base (1M) | **1,7 GB** | `VACUUM` copiaba el `.db` entero en RAM → OOM proyectado a 10M. Durante la ingesta el RSS era 7–8 MB; el pico estaba en `finalize` (FTS 250 MB → rollups 330 MB → VACUUM 1,7 GB). |
| Tras P1 (2M) | **676 MB** | VACUUM fuera del camino caliente (opt-in `init --compact`), `temp_store=FILE`, `foreign_keys=OFF` en ingesta, `cache_size=-262144` (256 MB), commits por lotes de 100k. |
| `sync` incremental (2M) | **578 MB** | Sin reconstrucción completa; solo el delta. |

La memoria pasó de ~tamaño-del-`.db` (ilimitada, con riesgo de OOM) a **acotada**.

## Ingesta (`init`)

| Métrica | Antes | Ahora |
|---|---|---|
| Curva (ev/s por tramos de 100k) | 39k → 13k (**3× de caída** a 1M) | 36,5k → 31,8k (**~1,3×** a 2M) |
| Tiempo total | 64 s / 1M | 62,9 s / **2M** |
| Tamaño `.db` | — | 1,8 GB / 2M (~0,9 KB/evento) |

La degradación residual (~1,3×) viene de la PK TEXT aleatoria de
`events`/`touches`/`labels`; la ataca P4 (`source_id` entero).

## Sync (`sync`)

| Caso | Antes | Ahora |
|---|---|---|
| Sin cambios (no-op) | 9,8 s | **6 ms** |
| +1000 eventos sobre 2M | 10,4 s | **0,9 s** |

El no-op era O(N) por un bug (`synced_any` siempre activo) + `rebuild_fts` +
`build_rollups` + `VACUUM` completos. Ahora: guard `n>0`, FTS y rollups
incrementales por watermark de rowid, y `ANALYZE` (O(N), ~15 s/2M) **solo** en
reconstrucción (init/divergencia); en sync incremental se usa `PRAGMA optimize`.

## Consultas (P3, aditivo sin reindex)

Optimizaciones verificadas byte-idénticas al binario pre-P3 sobre jsonl (408 ev,
incl. eventos sin tiempo y duplicados exactos) y sobre un repo git (12 consultas,
paridad git intacta). A 500k ya son sub-0,1 s; el beneficio es **asintótico**
(evitar O(N)), confirmado por `EXPLAIN QUERY PLAN`:

| Consulta | Cambio | Plan |
|---|---|---|
| `hotspots`/`churn` (`since==0`) | Quita el JOIN a `events` (eran ~1M lookups por PK TEXT). Guard de epoch negativo (pre-1970 → cae al JOIN) | sin `SEARCH events (id=?)` ni BLOOM FILTER |
| `top level`/`top actor` | Índices covering `idx_events_level`/`idx_events_actor` | sin `SCAN events` |
| `timeline` | Lee de `rollups` (O(#buckets)) cuando es byte-idéntico; si no, cae a `events` | `SCAN rollups` vs `idx_events_at` |
| `similar` | Bandas SimHash `idx_events_sh0..3`; exacto hasta dist 3, escaneo a dist ≥4 (default) | `MULTI-INDEX OR` |

## Qué sigue O(N) (P4)

`top attr:<key>` (json_extract), `top entity` (join a events), `timeline --by`,
`similar` a dist ≥4, `hotspots`/`churn` con ventana. Plan P4 (schema v5, un
reindex): `source_id` → entero, columnas `added/deleted` en `touches`,
`dim_rollups` para `top`/`timeline --by`.

## Reproducción

Determinismo: dos `init` del mismo fixture con `CHRONO_NOW` fijo dan un `.db`
byte-idéntico (SHA-256 igual). Paridad: `init` de una vez == `init` + `sync`
partido (dump de rollups/consultas idéntico). Sin sharding (ver `DECISIONS.md`
§25): partir la BD rompería `correlate`/`trace`/`events`.
