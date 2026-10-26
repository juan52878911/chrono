#!/usr/bin/env python3
"""Genera un JSONL sintético de logs para medir chrono a escala.

12 patrones de mensaje, 20 servicios, 8 hosts, ~1M eventos en 24 h con la semilla fija:
mismo N, mismo fichero byte a byte. Los ids hex (`%08x`) pueden salir sin dígitos a
propósito: es el caso que fragmenta las plantillas (C6 en docs/PROPUESTA-OPTIMIZACION.md).
Uso: gen.py N salida.jsonl"""
import json, random, sys

N, OUT = int(sys.argv[1]), sys.argv[2]
random.seed(7)
svcs = [f"svc-{i}" for i in range(20)]
hosts = [f"host-{i}" for i in range(8)]
tpls = ["request GET /api/users/{n} took {n}ms", "connection to 10.0.{n}.{n}:5432 refused",
        "retry {n}/5 for job {h}", "cache miss key=user:{n}", "pyo3 build finished in {n}s",
        "worker {n} exited with code {n}", "GC pause {n}ms", "received fast shutdown request",
        "checkpoint complete: wrote {n} buffers", "auth token {h} expired", "queue depth {n}",
        "timeout after {n}ms calling svc-{n}"]
levels = ["INFO"] * 85 + ["WARN"] * 10 + ["ERROR"] * 4 + ["FATAL"]
t0 = 1760000000000
with open(OUT, "w") as f:
    for i in range(N):
        ts = t0 + i * 86  # ~1M eventos en 24 h
        msg = random.choice(tpls).replace("{n}", str(random.randint(1, 99999)), 3).replace("{h}", "%08x" % random.getrandbits(32))
        f.write(json.dumps({"ts": ts, "level": random.choice(levels), "service": random.choice(svcs),
                            "host": random.choice(hosts), "trace_id": "%016x" % random.getrandbits(64), "msg": msg}) + "\n")
