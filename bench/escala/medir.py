#!/usr/bin/env python3
"""Bench de escala: tiempo, RSS, tamaño del índice y bytes de respuesta a N eventos.

Genera el fixture con gen.py (o lo reutiliza si ya existe con ese N), indexa desde cero y
mide cada consulta por el CLI (tiempo de pared y RSS máximo del proceso, con os.wait4) y
por el MCP (bytes del texto que recibiría el agente). También mide el binario y los
esquemas de `tools/list`. Escribe un JSON; con --base compara contra otro y marca lo que
empeora más de un 10 %.
Uso: medir.py BIN [--n 1000000] [--dir DIR] [--out res.json] [--base anterior.json]"""
import argparse, json, os, shutil, sqlite3, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
ap = argparse.ArgumentParser()
ap.add_argument("bin")
ap.add_argument("--n", type=int, default=1_000_000)
ap.add_argument("--dir", default=os.path.join(os.environ.get("TMPDIR", "/tmp"), "chrono-escala"))
ap.add_argument("--out")
ap.add_argument("--base")
a = ap.parse_args()
BIN = os.path.abspath(a.bin)
os.makedirs(a.dir, exist_ok=True)
corpus = os.path.join(a.dir, f"events-{a.n}.jsonl")
db = os.path.join(a.dir, "index.db")
# ru_maxrss viene en bytes en macOS y en KB en Linux.
RSS_UNIT = 1 if sys.platform == "darwin" else 1024

def run(args, stdin=None):
    """Ejecuta chrono y devuelve (segundos, RSS máx en MB, stdout en bytes)."""
    t = time.perf_counter()
    p = subprocess.Popen([BIN, *args], cwd=a.dir, stdin=subprocess.PIPE if stdin else subprocess.DEVNULL,
                         stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    if stdin:  # sin communicate(): recogería al hijo y wait4 ya no tendría su rusage
        p.stdin.write(stdin.encode())
        p.stdin.close()
    out = p.stdout.read()
    _, status, ru = os.wait4(p.pid, 0)
    if os.waitstatus_to_exitcode(status) != 0:
        sys.exit(f"falló: chrono {' '.join(args)}")
    return round(time.perf_counter() - t, 3), round(ru.ru_maxrss * RSS_UNIT / 1e6, 1), out

def mcp(calls):
    """Llama al MCP en una sola sesión y devuelve el texto de cada respuesta, por id."""
    reqs = [{"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}]
    reqs += [{"jsonrpc": "2.0", "id": i + 1, **c} for i, c in enumerate(calls)]
    _, _, out = run(["mcp", "--db", db], "\n".join(json.dumps(r) for r in reqs) + "\n")
    return {r["id"]: r.get("result") for r in map(json.loads, out.decode().splitlines()) if r.get("id")}

if not os.path.exists(corpus):
    subprocess.run([sys.executable, "-I", os.path.join(HERE, "gen.py"), str(a.n), corpus], check=True)
for suf in ("", "-wal", "-shm"):
    if os.path.exists(db + suf):
        os.remove(db + suf)

res = {"version": run(["--version"])[2].decode().strip(), "n": a.n,
       "binario_bytes": os.path.getsize(shutil.which(BIN) or BIN)}
seg, rss, _ = run(["add", corpus, "--db", db])
res["ingesta"] = {"s": seg, "rss_mb": rss, "db_bytes": os.path.getsize(db),
                  "bytes_por_evento": round(os.path.getsize(db) / a.n, 1)}
con = sqlite3.connect(db)
res["plantillas"] = con.execute("select count(*) from templates").fetchone()[0]
con.close()
if shutil.which("sqlite3"):  # dbstat suele venir en el sqlite3 del sistema, no en el de Python
    q = "select name, sum(pgsize) from dbstat group by name order by 2 desc limit 12"
    out = subprocess.run(["sqlite3", "-separator", "\t", db, q], capture_output=True, text=True).stdout
    res["dbstat_mb"] = {n: round(int(b) / 1e6, 1) for n, b in (l.split("\t") for l in out.splitlines())}
res["sync_noop_s"] = run(["sync", "--db", db])[0]

CONSULTAS = [
    ["informe"], ["top", "entity"], ["top", "attr:host"], ["top", "level"],
    ["patterns", "--level", "ERROR,FATAL"], ["patterns", "--entity", "svc-3"], ["patterns"],
    ["timeline", "--bucket", "1h"], ["timeline", "--bucket", "1h", "--by", "level"],
    ["precede", "--grep", "refused", "--level", "ERROR"], ["precede", "--level", "FATAL"],
    ["events", "--level", "FATAL", "--limit", "50"], ["search", "refused"],
]
res["cli"] = {}
for c in CONSULTAS:
    seg, rss, out = run([*c, "--db", db])
    res["cli"][" ".join(c)] = {"s": seg, "rss_mb": rss, "bytes": len(out)}

# Lo que recibe el agente: esquemas y respuestas por defecto del MCP.
LLAMADAS = [("informe", {}), ("top", {"dim": "entity"}), ("events", {"level": "ERROR"}),
            ("precede", {"level": "ERROR", "grep": "refused"}), ("patterns", {"level": "ERROR,FATAL"}),
            ("timeline", {"by": "level"})]
r = mcp([{"method": "tools/list"}] + [{"method": "tools/call", "params": {"name": n, "arguments": args}}
                                      for n, args in LLAMADAS])
tools = r[1]["tools"]
res["mcp"] = {"tools": len(tools), "esquemas_bytes": len(json.dumps(tools, ensure_ascii=False).encode()),
              "respuestas_bytes": {f"{n} {json.dumps(args)}": len(r[i + 2]["content"][0]["text"].encode())
                                   for i, (n, args) in enumerate(LLAMADAS)}}

txt = json.dumps(res, indent=2, ensure_ascii=False)
if a.out:
    open(a.out, "w").write(txt + "\n")
print(txt)

if a.base:
    base = json.load(open(a.base))
    def plano(d, pre=""):
        for k, v in d.items():
            if isinstance(v, dict):
                yield from plano(v, f"{pre}{k} · ")
            elif isinstance(v, (int, float)):
                yield f"{pre}{k}", v
    antes = dict(plano(base))
    print(f"\n{'métrica':<60} {'antes':>12} {'ahora':>12}  cambio")
    for k, v in plano(res):
        if k in antes and antes[k] != v:
            d = (v - antes[k]) / antes[k] * 100 if antes[k] else float("inf")
            marca = "  ⚠" if d > 10 and not k.startswith(("n", "tools")) else ""
            print(f"{k:<60} {antes[k]:>12} {v:>12}  {d:+.0f} %{marca}")
