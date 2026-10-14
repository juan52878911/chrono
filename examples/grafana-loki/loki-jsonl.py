#!/usr/bin/env python3
"""Vuelca logs de Loki a JSONL ({ts, service, level, msg, labels}) para `chrono add cmd:loki`.

Uso: loki-jsonl.py '<selector LogQL>' [inicio] [fin]
     inicio/fin: RFC3339 (2026-10-13T22:00:00Z) o relativo (now-24h, now-90m, now). Por defecto now-24h → now.

Entorno:
  LOKI_URL       URL base de la API de Loki, o la del proxy de Grafana:
                 https://grafana.ejemplo/api/datasources/proxy/uid/<uid-de-loki>
  LOKI_TOKEN     Bearer token (p. ej. una cuenta de servicio Viewer de Grafana), o
  LOKI_TOKEN_CMD un comando que lo imprime (p. ej. leerlo del llavero), para no dejarlo en un fichero.
  LOKI_LABELS    etiquetas a copiar a cada evento (por defecto service,level,container).

Pagina hacia delante en bloques de 5000 líneas, sin duplicados, y solo usa la biblioteca estándar.
"""
import json, os, re, shlex, subprocess, sys, time, urllib.parse, urllib.request


def to_ns(v: str) -> int:
    v = v.strip()
    m = re.fullmatch(r"now(?:-(\d+)([smhd]))?", v)
    if m:
        secs = int(m.group(1) or 0) * {"s": 1, "m": 60, "h": 3600, "d": 86400}.get(m.group(2) or "s", 1)
        return int((time.time() - secs) * 1e9)
    from datetime import datetime
    return int(datetime.fromisoformat(v.replace("Z", "+00:00")).timestamp() * 1e9)


def token() -> str:
    if os.environ.get("LOKI_TOKEN"):
        return os.environ["LOKI_TOKEN"]
    cmd = os.environ.get("LOKI_TOKEN_CMD")
    if cmd:
        return subprocess.run(shlex.split(cmd), capture_output=True, text=True, check=True).stdout.strip()
    return ""


def main() -> None:
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    sel = sys.argv[1]
    start = to_ns(sys.argv[2] if len(sys.argv) > 2 else "now-24h")
    end = to_ns(sys.argv[3] if len(sys.argv) > 3 else "now")
    base = os.environ["LOKI_URL"].rstrip("/") + "/loki/api/v1/query_range"
    keep = os.environ.get("LOKI_LABELS", "service,level,container").split(",")
    headers = {"Authorization": f"Bearer {t}"} if (t := token()) else {}
    seen: set = set()
    cur = start
    while True:
        q = urllib.parse.urlencode({"query": sel, "start": cur, "end": end, "limit": 5000, "direction": "forward"})
        req = urllib.request.Request(f"{base}?{q}", headers=headers)
        streams = json.load(urllib.request.urlopen(req, timeout=60))["data"]["result"]
        rows, total = [], 0
        for s in streams:
            lab = s["stream"]
            for ts, line in s["values"]:
                total += 1
                k = (ts, tuple(sorted(lab.items())), line)
                if k in seen:
                    continue
                seen.add(k)
                rows.append((int(ts), lab, line))
        rows.sort(key=lambda r: r[0])
        for ts, lab, line in rows:
            ev = {"ts": ts // 1_000_000, "msg": line}
            ev.update({k: lab.get(k, "") for k in keep})
            print(json.dumps(ev, ensure_ascii=False))
        if total < 5000 or not rows:
            break
        cur = rows[-1][0]  # inclusivo; los repetidos se descartan con `seen`


if __name__ == "__main__":
    main()
