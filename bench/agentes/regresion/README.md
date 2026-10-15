# Bench de regresión

Dos niveles, del más barato al más caro.

## 1. Determinista (CI, sin LLM)

```sh
python3 -I bench/agentes/regresion/oro_chrono.py rust/target/debug/chrono
```

Indexa `corpus/logs-sandbox.jsonl.gz` (24 h de logs reales de un sandbox, 6 587 líneas,
anonimizadas con `anonimizar.py`) y comprueba que las consultas que usaría un agente dan el
**oro**: `oro.json`, calculado por `oro_ref.py` con Python plano, sin chrono. Cubre las 4
preguntas del bench (cifras por nivel y servicio, top-3 de errores, causa de una excepción,
reinicios y su precursor) y el determinismo: reindexar desde cero da el mismo `informe`, y el
CLI y el MCP dan el mismo texto. Corre en el CI en cada push.

## 2. Con agentes (a mano, cuesta dinero)

```sh
# MiniMax (clave por variable o comando)
MOTOR=minimax MINIMAX_API_KEY_CMD='security find-generic-password -s ws/MINIMAX_API_KEY -w' \
  CHRONO_BIN=$PWD/rust/target/debug/chrono bench/agentes/regresion/agentes.sh skill 1
# Cuenta de Claude de la máquina
MOTOR=claude MODELO=claude-sonnet-5-5 bench/agentes/regresion/agentes.sh sin-chrono 1
python3 -I bench/agentes/regresion/corregir.py bench/agentes/regresion/resultados/<fecha>
python3 -I bench/agentes/regresion/corregir.py <dir> --gate bench/agentes/regresion/umbrales.json
```

Brazos: `sin-chrono` (Read/Grep/utilidades de texto), `skill` (plugin + datos ya en chrono),
`intermediario` (plugin + `Read` interceptado). Cada sesión en su propio directorio. Usa al
menos 5 sesiones por brazo: con 3 la varianza de un mismo brazo llega a ±1 punto.

`corregir.py` aplica la rúbrica manual (4 preguntas de 1 punto) buscando en la respuesta las
cifras y hechos del oro, y marca señales de cifra falsa (totales contados por texto, top-3
42/18/12, número de reinicios distinto). **Es heurístico**: calibrado contra 22 respuestas
corregidas a mano, error absoluto medio 0,28 y máximo 0,75 puntos sobre 4. Sirve para detectar
regresiones de ≥ 0,5; para comparar brazos cercanos, revisa a mano.
