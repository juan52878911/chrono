# Logs de Loki/Grafana en chrono

Un MCP de Grafana devuelve como mucho 100 líneas por consulta: sirve para leer un detalle,
no para contar ni rankear. Con esta fuente de comando, chrono guarda los logs de la ventana
y responde con cifras exactas (`top`, `patterns`, `timeline`, `events`) sobre todos ellos.

## Instalar

```sh
mkdir -p .chrono/scripts
cp examples/grafana-loki/loki-jsonl.py .chrono/scripts/
cp examples/grafana-loki/config.json   .chrono/config.json   # o fusiona "commands" con el tuyo
export LOKI_URL="https://grafana.ejemplo/api/datasources/proxy/uid/loki"
export LOKI_TOKEN_CMD="security find-generic-password -s grafana-token -w"   # o LOKI_TOKEN=…
chrono add cmd:loki        # primera carga
chrono sync                # después: solo lo nuevo (el plugin de Claude Code lo hace al empezar la sesión)
```

Ajusta en `config.json` el selector (qué servicios) y la ventana (`now-24h`). Para una
ventana fija pon fechas RFC3339. Cada evento lleva `service` como entidad y `level` como nivel,
así que `chrono top entity --level FATAL` dice en qué servicios caen los FATAL.

## Preguntas típicas

| Pregunta | Consulta |
|---|---|
| Cuántas líneas por nivel | `chrono top level --compact` |
| En qué servicios caen los errores | `chrono top entity --level FATAL,ERROR,CRITICAL --compact` |
| Los 3 errores más repetidos | `chrono patterns --level FATAL,ERROR,CRITICAL --limit 3 --compact` |
| Cuándo empezó / picos | `chrono timeline --grep "<mensaje>" --bucket 10m --compact` |
| Qué pasó justo antes de X | `chrono events --entity postgres --until <ts> --limit 30 --compact` |
| Traza completa de una excepción | `chrono events --entity product_service --since <ts> --limit 80 --compact` |
