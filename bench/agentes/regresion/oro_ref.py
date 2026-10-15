#!/usr/bin/env python3
"""Calcula el ORO del corpus con Python plano, SIN chrono (para poder juzgar a chrono).
Uso: oro_ref.py corpus/logs-sandbox.jsonl.gz > oro.json"""
import collections, gzip, json, sys
from datetime import datetime, timezone

rows = [json.loads(l) for l in gzip.open(sys.argv[1], "rt")]
lv = lambda r: r.get("level") or ""
def iso_min(ms):
    return datetime.fromtimestamp(ms / 1000, timezone.utc).strftime("%H:%M")

niveles = {}
for L in ["FATAL", "ERROR", "CRITICAL", "WARNING"]:
    sel = [r for r in rows if lv(r) == L]
    niveles[L] = {"total": len(sel), "servicios": dict(collections.Counter(r["service"] for r in sel).most_common())}
graves = [r for r in rows if lv(r) in ("FATAL", "ERROR", "CRITICAL")]
def cuenta(sub):
    return sum(1 for r in graves if sub in r["msg"])
top3 = [
    {"clave": "terminating connection due to administrator command", "veces": cuenta("terminating connection due to administrator command")},
    {"clave": "TimescaleDB Background Worker Scheduler", "veces": cuenta("TimescaleDB Background Worker Scheduler")},
    {"clave": "TimescaleDB Background Worker Launcher", "veces": cuenta("TimescaleDB Background Worker Launcher")},
]
ps = [r for r in rows if r["service"] == "product_service"]
excepcion = next(m for m in (r["msg"] for r in ps) if "AttributeError" in m and "import_source_id" in m)
endpoint = next(r["msg"] for r in ps if "500 Internal Server Error" in r["msg"] and "DELETE" in r["msg"])
reinicios = sorted(r["ts"] for r in rows if r.get("container") == "crm_aura_sandbox_postgres" and "received fast shutdown request" in r["msg"])
mismatch = sorted(r["ts"] for r in rows if "catalog version mismatch" in r["msg"])
precede = all(any(0 < (t - m) / 1000 <= 120 for m in mismatch) for t in reinicios)
oro = {
    "lineas": len(rows),
    "p1_niveles": niveles,
    "p1_sin_nivel_texto": {L: sum(1 for r in rows if not lv(r) and f"{L}:" in r["msg"]) for L in ["FATAL", "ERROR", "WARNING"]},
    "p2_top3": top3,
    "p3": {"endpoint": "DELETE /api/collections/1", "excepcion": "AttributeError: type object 'ImportJob' has no attribute 'import_source_id'",
           "veces": sum(1 for r in ps if "500 Internal Server Error" in r["msg"] and "DELETE /api/collections/1" in r["msg"]),
           "comprobado": "import_source_id" in excepcion and "DELETE /api/collections/1" in endpoint},
    "p4": {"reinicios": len(reinicios), "horas_utc": [iso_min(t) for t in reinicios],
           "preflight_horas_utc": [iso_min(t) for t in mismatch],
           "precursor": "catalog version mismatch, expected \"2.30.0\" seen \"2.29.0\"", "precede_a_todos": precede},
}
print(json.dumps(oro, ensure_ascii=False, indent=1))
