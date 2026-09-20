# chrono — port a Rust (en desarrollo)

Reescritura de chrono en Rust para máximo rendimiento y control de memoria sobre
trazas de millones de líneas. El binario Go (raíz del repo, v0.1.1) sigue siendo
el estable hasta que este port alcance paridad (fase R2). Ver el diseño completo
en [`../docs/DESIGN-GENERAL-CORE.md`](../docs/DESIGN-GENERAL-CORE.md).

## Estado

- **R0 · Cimientos** ✅ — `chrono-core`: dominio `Event`/`Touch`/`Link`/`Label`
  (convención `entity`/`id`) y puertos `Source`/`Cursor`/`Classifier`. Sin dependencias.
- R1 — store SQLite v2 + adaptador git + paridad con Go. Pendiente.
- R2 — resto de consultas + MCP + i18n → sustituye al binario Go. Pendiente.
- R3 — adaptadores (jsonl/csv/textlog/changelog/journald) + multi-fuente + rollups.
- R4 — JEV Level-1 (features + inferencia i16) + trainer + eval.
- R5 — escala medida (bandas SimHash, bench, caps reales).

## Compilar / probar

```sh
cd rust
cargo test
```
