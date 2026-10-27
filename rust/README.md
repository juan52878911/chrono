# chrono — workspace Rust

chrono en Rust, para máximo rendimiento y control de memoria sobre trazas de
millones de líneas. Es el único chrono: el binario Go original se retiró al
alcanzar paridad (fase R2). Ver el diseño completo en
[`../docs/DESIGN-GENERAL-CORE.md`](../docs/DESIGN-GENERAL-CORE.md).

## Estado

- **R0 · Cimientos** ✅ — `chrono-core`: dominio `Event`/`Touch`/`Link`/`Label`
  (convención `entity`/`id`) y puertos `Source`/`Cursor`/`Classifier`. Sin dependencias.
- R1 · store SQLite + adaptador git + paridad con Go ✅
- R2 · resto de consultas + MCP + i18n → sustituye al binario Go ✅
- R3 — adaptadores (jsonl/csv/textlog/changelog/journald) + multi-fuente + rollups.
- R4 — JEV Level-1 (features + inferencia i16) + trainer + eval.
- R5 — escala medida (bandas SimHash, bench, caps reales).

## Compilar / probar

```sh
cd rust
cargo test
```
