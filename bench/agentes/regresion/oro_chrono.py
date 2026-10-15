#!/usr/bin/env python3
"""Test de regresión DETERMINISTA (sin LLM): chrono contra el oro del corpus.

1. Indexa el corpus en un directorio temporal y comprueba que las consultas que un
   agente usaría para las 4 preguntas devuelven exactamente el oro (oro.json, que
   calcula oro_ref.py sin chrono).
2. Determinismo: reindexar desde cero en la misma ruta y repetir `informe` da los
   mismos bytes; `informe` por el CLI y por el MCP da el mismo texto.
Uso: oro_chrono.py [ruta-al-binario-chrono]   (sale con 1 si algo no cuadra)"""
import gzip, json, os, shutil, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
BIN = sys.argv[1] if len(sys.argv) > 1 else "chrono"
oro = json.load(open(os.path.join(HERE, "oro.json")))
fallos = []

def check(nombre, obtenido, esperado):
    ok = obtenido == esperado
    print(f"{'ok ' if ok else 'MAL'} {nombre}: {obtenido}" + ("" if ok else f"  (esperado {esperado})"))
    if not ok:
        fallos.append(nombre)

tmp = tempfile.mkdtemp(prefix="chrono-oro-")
corpus = os.path.join(tmp, "logs-sandbox.jsonl")
with gzip.open(os.path.join(HERE, "corpus", "logs-sandbox.jsonl.gz"), "rt") as f, open(corpus, "w") as out:
    out.write(f.read())
db = os.path.join(tmp, "index.db")

def build():
    for suf in ("", "-wal", "-shm"):
        if os.path.exists(db + suf):
            os.remove(db + suf)
    subprocess.run([BIN, "add", corpus, "--db", db], cwd=tmp, check=True, capture_output=True)

def q(*args):
    out = subprocess.run([BIN, *args, "--db", db], cwd=tmp, check=True, capture_output=True, text=True).stdout
    return json.loads(out)["result"]

def mcp(name, arguments):
    reqs = [{"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
            {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": arguments}}]
    out = subprocess.run([BIN, "mcp", "--db", db], cwd=tmp, input="\n".join(json.dumps(r) for r in reqs) + "\n",
                         check=True, capture_output=True, text=True).stdout.strip().splitlines()[-1]
    return json.loads(out)["result"]["content"][0]["text"]

build()
# P1: totales y servicios por nivel (nivel de la fuente).
lv = {t["value"]: t["count"] for t in q("top", "level")["top"]}
for L, g in oro["p1_niveles"].items():
    check(f"P1 total {L}", lv.get(L, 0), g["total"])
    svc = {t["value"]: t["count"] for t in q("top", "entity", "--level", L)["top"]}
    check(f"P1 servicios {L}", svc, g["servicios"])
# Líneas sin nivel cuyo texto lo dice (sources) — se informan aparte, no se suman.
src = q("sources")["sources"][0]
inferidas = dict(p.split("=") for p in src.get("unlabeled_inferred", "").split())
check("P1 sin nivel (texto)", {k: int(v) for k, v in inferidas.items()}, oro["p1_sin_nivel_texto"])
# P2: top-3 de mensajes graves.
pats = q("patterns", "--level", "FATAL,ERROR,CRITICAL", "--limit", "3")["patterns"]
check("P2 top3", [p["count"] for p in pats], [t["veces"] for t in oro["p2_top3"]])
check("P2 plantillas", all(t["clave"] in p["template"] for p, t in zip(pats, oro["p2_top3"])), True)
# P3: endpoint y excepción, localizables con events.
ev = q("events", "--entity", "product_service", "--grep", "DELETE /api/collections/1", "--limit", "10")["events"]
check("P3 peticiones 500", sum("500 Internal" in e["title"] for e in ev), oro["p3"]["veces"])
exc = q("events", "--entity", "product_service", "--grep", "import_source_id", "--limit", "5")["events"]
check("P3 excepción", any("AttributeError" in e["title"] for e in exc), True)
# P4: episodios del postgres principal y su precursor.
pre = q("precede", "--grep", "received fast shutdown request", "--attr", "container:crm_aura_sandbox_postgres")
check("P4 reinicios", pre["episodes_total"], oro["p4"]["reinicios"])
check("P4 horas", [e["start"][11:16] for e in pre["episodes"]], oro["p4"]["horas_utc"])
mism = [c for c in pre["common_precursors"] if "catalog version mismatch" in c["template"]]
check("P4 precursor en todos", mism[0]["before_episodes"] if mism else None, f"{oro['p4']['reinicios']}/{oro['p4']['reinicios']}")
# Determinismo.
inf1 = subprocess.run([BIN, "informe", "--db", db], cwd=tmp, check=True, capture_output=True, text=True).stdout
build()
inf2 = subprocess.run([BIN, "informe", "--db", db], cwd=tmp, check=True, capture_output=True, text=True).stdout
check("determinismo: reindexar da el mismo informe", inf1 == inf2, True)
check("CLI y MCP dan el mismo informe", inf1.strip() == mcp("informe", {}).strip(), True)
shutil.rmtree(tmp, ignore_errors=True)
print(f"\n{'TODO BIEN' if not fallos else 'FALLOS: ' + ', '.join(fallos)}")
sys.exit(1 if fallos else 0)
