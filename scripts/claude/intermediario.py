#!/usr/bin/env python3
"""chrono como intermediario entre las herramientas y el modelo (hooks de Claude Code).

Solo actúa donde una regla de `.chrono/intermediario.json` lo pide y por encima de
sus límites; en cualquier otro caso sale sin decir nada y la herramienta sigue igual.
La configuración la escribe la skill `chrono:chrono-intermediario` tras analizar las
herramientas de la sesión.

  intermediario.py post   PostToolUse (herramientas MCP): sustituye la salida
                          (`updatedMCPToolOutput`) por cifras exactas + muestra.
  intermediario.py read   PreToolUse (Read): un fichero de datos grande se indexa
                          y en vez de leerlo entero se devuelve su resumen.

Adaptadores:
  loki     vuelve a pedir la MISMA consulta LogQL a la API de Loki paginando (sin el
           tope de líneas del MCP), la guarda en chrono y resume TODAS las líneas.
  records  la salida ya trae los datos (JSON con una lista de registros, o JSONL):
           la guarda en chrono y la resume. Para MCPs de BD, CI, tickets…

Nunca rompe la herramienta: ante cualquier error, sale con 0 y sin salida.
Desactivar: CHRONO_INTERMEDIARIO=0. Diagnóstico: CHRONO_INTERMEDIARIO_LOG=<fichero>.
"""
import hashlib, json, os, re, shlex, subprocess, sys, time, traceback
import urllib.parse, urllib.request
from collections import Counter
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
METRIC_RE = re.compile(
    r"^\s*(sum|avg|min|max|count|topk|bottomk|stddev|stdvar|sort|sort_desc|quantile_over_time|"
    r"count_over_time|rate|bytes_rate|bytes_over_time|absent_over_time|avg_over_time|sum_over_time|"
    r"max_over_time|min_over_time|first_over_time|last_over_time|stddev_over_time|label_replace|vector)\b"
)
PREFIX_RE = re.compile(r"^\d{4}-\d\d-\d\d[ T]\d\d:\d\d:\d\d(?:[.,]\d+)?(?: ?(?:UTC|Z|[+-]\d\d:?\d\d))?\s*(?:\[\d+\]\s*)?")
VAR_RE = re.compile(
    r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b|\b\d{1,3}(?:\.\d{1,3}){3}(?::\d+)?\b|"
    r"\b(?=[0-9a-f]*\d)(?=[0-9a-f]*[a-f])[0-9a-f]{8,}\b|\d+(?:\.\d+)?",
    re.I,
)


def log(msg):
    p = os.environ.get("CHRONO_INTERMEDIARIO_LOG")
    if p:
        with open(p, "a") as f:
            f.write(f"{datetime.now().isoformat()} {msg}\n")


# ── Configuración e índice ────────────────────────────────────────────────────
def find_root(cwd):
    d = os.path.abspath(cwd or os.getcwd())
    while True:
        if os.path.isfile(os.path.join(d, ".chrono", "intermediario.json")):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent


def load_config(root):
    with open(os.path.join(root, ".chrono", "intermediario.json")) as f:
        return json.load(f)


def chrono_bin():
    try:
        out = subprocess.run([os.path.join(HERE, "ensure-chrono.sh")], capture_output=True, text=True,
                             timeout=60, env={**os.environ, "CHRONO_AUTO_INSTALL": os.environ.get("CHRONO_AUTO_INSTALL", "0")})
        return out.stdout.strip() or None
    except Exception:
        return None


def ingest(root, name, rows):
    """Añade `rows` (dicts con ts/msg/…) a `.chrono/mcp/<name>.jsonl` sin duplicados y
    sincroniza el índice del proyecto. Devuelve el id de la fuente para `source`."""
    bin_ = chrono_bin()
    if not bin_:
        return None
    d = os.path.join(root, ".chrono", "mcp")
    os.makedirs(d, exist_ok=True)
    path = os.path.join(d, f"{name}.jsonl")
    keys_path = path + ".keys"
    seen = set()
    if os.path.exists(keys_path):
        with open(keys_path) as f:
            seen = set(l.strip() for l in f)
    new = []
    for r in rows:
        k = hashlib.sha1(json.dumps(r, sort_keys=True, ensure_ascii=False).encode()).hexdigest()[:20]
        if k not in seen:
            seen.add(k)
            new.append((k, r))
    if new:
        with open(path, "a") as f, open(keys_path, "a") as kf:
            for k, r in new:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
                kf.write(k + "\n")
    db = os.path.join(root, ".chrono", "index.db")
    known = False
    if os.path.exists(db):
        out = subprocess.run([bin_, "sources", "--compact", "--db", db], capture_output=True, text=True, timeout=60).stdout
        known = path in out or f"{name}.jsonl" in out
    cmd = [bin_, "sync", "--db", db] if known else [bin_, "add", path, "--db", db]
    subprocess.run(cmd, cwd=root, capture_output=True, text=True, timeout=300)
    return f"jsonl:{path}"


# ── Resumen exacto ────────────────────────────────────────────────────────────
def template(msg):
    first = msg.split("\n", 1)[0]
    return VAR_RE.sub("<N>", PREFIX_RE.sub("", first)).strip()[:160]


def iso(ms):
    return datetime.fromtimestamp(ms / 1000, timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%f")[:-3] + "Z"


def summary(rows, *, total_note, source_id, sample, sample_note, dims=("level", "service", "container")):
    lines = [total_note]
    if rows and "ts" in rows[0]:
        lines.append(f"ventana real de los datos: {iso(min(r['ts'] for r in rows))} → {iso(max(r['ts'] for r in rows))}")
    for dim in dims:
        c = Counter(str(r.get(dim, "")) for r in rows if r.get(dim) not in (None, ""))
        if c and len(c) < len(rows):
            top = c.most_common(15)
            rest = len(c) - len(top)
            lines.append(f"{dim}: " + ", ".join(f"{k}={v}" for k, v in top) + (f" (+{rest} valores más)" if rest > 0 else ""))
    msgs = [str(r.get("msg", "")) for r in rows]
    levels = Counter(str(r.get("level", "")) for r in rows if r.get("level"))
    if 1 < len(levels) <= 8 or (len(levels) == 1 and len(rows) > 1):
        # Con niveles, el desglose por nivel va PRIMERO: «los errores más repetidos» se cuentan sobre
        # las líneas de ese nivel, no sobre todas las que casan con el texto de la consulta.
        lines.append("mensajes más repetidos POR NIVEL (veces · plantilla):")
        for lv, _ in levels.most_common():
            if lv.upper() in ("INFO", "DEBUG", "TRACE", "LOG"):
                continue
            c = Counter(template(str(r.get("msg", ""))) for r in rows if r.get("level") == lv)
            lines.append(f"  {lv}: " + " | ".join(f"{n}× {t[:110]}" for t, n in c.most_common(5)))
        sin = sum(1 for r in rows if not r.get("level"))
        if sin:
            lines.append(f"  ({sin} líneas de la consulta no tienen nivel: no cuentan en ningún nivel)")
    elif msgs and any(msgs):
        lines.append("mensajes más repetidos (veces · plantilla):")
        for t, n in Counter(template(m) for m in msgs if m).most_common(10):
            lines.append(f"  {n:>6} · {t}")
    if sample:
        lines.append(sample_note)
        lines.extend(sample)
    if source_id:
        lines.append(
            f"Las {len(rows)} filas están en chrono (source={source_id}): para contar, rankear o leer en orden "
            "usa sus tools top/patterns/timeline/events con filtros level, entity (servicio), attr "
            "(p. ej. container:<nombre>), grep, since/until. Las cifras de arriba son exactas: no las recuentes."
        )
    return "\n".join(lines)


def fmt_row(r, width=400):
    ts = iso(r["ts"]) if isinstance(r.get("ts"), (int, float)) else str(r.get("ts", ""))
    who = r.get("container") or r.get("service") or ""
    return f"{ts} {who} {r.get('level', '') or '-'} {str(r.get('msg', ''))[:width]}"


# ── Adaptador loki ────────────────────────────────────────────────────────────
def to_ns(v, default):
    v = (v or default).strip()
    m = re.fullmatch(r"now(?:-(\d+)([smhd]))?", v)
    if m:
        secs = int(m.group(1) or 0) * {"s": 1, "m": 60, "h": 3600, "d": 86400}.get(m.group(2) or "s", 1)
        return int((time.time() - secs) * 1e9)
    return int(datetime.fromisoformat(v.replace("Z", "+00:00")).timestamp() * 1e9)


def loki_fetch(rule, logql, start_ns, end_ns, max_lines, deadline):
    url = (rule.get("url") or os.environ.get("LOKI_URL", "")).rstrip("/")
    if not url:
        raise RuntimeError("loki: falta url (regla) o LOKI_URL")
    tok = os.environ.get("LOKI_TOKEN", "")
    cmd = rule.get("token_cmd") or os.environ.get("LOKI_TOKEN_CMD")
    if not tok and cmd:
        tok = subprocess.run(shlex.split(cmd), capture_output=True, text=True, check=True, timeout=20).stdout.strip()
    headers = {"Authorization": f"Bearer {tok}"} if tok else {}
    keep = rule.get("labels", ["service", "level", "container"])
    rows, seen, cur, truncated = [], set(), start_ns, False
    while True:
        if time.time() > deadline or len(rows) >= max_lines:
            truncated = True
            break
        q = urllib.parse.urlencode({"query": logql, "start": cur, "end": end_ns, "limit": 5000, "direction": "forward"})
        req = urllib.request.Request(f"{url}/loki/api/v1/query_range?{q}", headers=headers)
        data = json.load(urllib.request.urlopen(req, timeout=max(5, deadline - time.time())))["data"]
        if data.get("resultType") != "streams":
            return None, False
        batch, total = [], 0
        for s in data["result"]:
            lab = s["stream"]
            for ts, line in s["values"]:
                total += 1
                k = (ts, tuple(sorted(lab.items())), line)
                if k not in seen:
                    seen.add(k)
                    r = {"ts": int(ts) // 1_000_000, "msg": line}
                    r.update({k2: lab.get(k2, "") for k2 in keep if lab.get(k2)})
                    batch.append((int(ts), r))
        batch.sort(key=lambda x: x[0])
        rows.extend(r for _, r in batch)
        if total < 5000 or not batch:
            break
        cur = batch[-1][0]
    return rows[:max_lines], truncated


def adapter_loki(rule, inp, root, deadline):
    logql = inp.get("logql") or inp.get("query") or ""
    if not logql or METRIC_RE.match(logql) or inp.get("queryType") == "instant":
        return None  # métricas: ya son exactas, no hay nada que mejorar
    start = to_ns(inp.get("startRfc3339") or inp.get("start"), "now-1h")
    end = to_ns(inp.get("endRfc3339") or inp.get("end"), "now")
    asked = int(inp.get("limit") or 10)
    rows, truncated = loki_fetch(rule, logql, start, end, int(rule.get("max_lines", 50000)), deadline)
    if rows is None:
        return None
    if len(rows) <= asked and len(rows) < int(rule.get("min_lines", 50)):
        return None  # cabe entera en la salida del MCP: no hay nada que corregir
    source_id = ingest(root, rule.get("name", "loki"), rows)
    backward = (inp.get("direction") or "backward") == "backward"
    n_sample = min(asked, int(rule.get("sample_lines", 40)), len(rows))
    picked = (rows[-n_sample:][::-1] if backward else rows[:n_sample]) if n_sample else []
    note = (f"[chrono · intermediario] La consulta casa con {len(rows)} líneas en la ventana pedida"
            + (" (tope del intermediario alcanzado: hay más)" if truncated else "")
            + (f"; el MCP solo devolvía {asked}." if len(rows) > asked else "; caben en la salida del MCP, pero sin agregar.")
            + f" Cifras EXACTAS sobre las {len(rows)}:")
    return summary(rows, total_note=note, source_id=source_id,
                   sample=[fmt_row(r) for r in picked],
                   sample_note=f"muestra: {len(picked)} líneas ({'las más recientes primero' if backward else 'las más antiguas primero'}):")


# ── Adaptador records ─────────────────────────────────────────────────────────
def response_text(resp):
    if isinstance(resp, str):
        return resp
    if isinstance(resp, dict) and "content" in resp:
        resp = resp["content"]
    if isinstance(resp, list):
        return "".join(b.get("text", "") for b in resp if isinstance(b, dict))
    return json.dumps(resp, ensure_ascii=False)


def find_records(obj):
    """La lista de dicts más larga dentro de un JSON (los registros)."""
    best = []
    stack = [obj]
    while stack:
        x = stack.pop()
        if isinstance(x, list):
            if len(x) > len(best) and x and all(isinstance(i, dict) for i in x[:50]):
                best = x
            stack.extend(i for i in x if isinstance(i, (list, dict)))
        elif isinstance(x, dict):
            stack.extend(v for v in x.values() if isinstance(v, (list, dict)))
    return best


def adapter_records(rule, tool, resp, root):
    text = response_text(resp)
    if len(text.encode()) < int(rule.get("min_bytes", 30000)):
        return None
    recs = []
    try:
        recs = find_records(json.loads(text))
    except Exception:
        for line in text.splitlines():
            try:
                o = json.loads(line)
                if isinstance(o, dict):
                    recs.append(o)
            except Exception:
                pass
    if len(recs) < int(rule.get("min_records", 20)):
        return None
    name = rule.get("name") or re.sub(r"[^A-Za-z0-9_.-]", "_", tool)
    tf, mf = rule.get("time_field"), rule.get("message_field")
    rows = []
    for r in recs:
        row = dict(r)
        if tf and tf in r:
            row["ts"] = r[tf]
        if mf and mf in r:
            row["msg"] = r[mf]
        rows.append(row)
    source_id = ingest(root, name, rows)
    dims = tuple(rule.get("dims", [k for k in ("level", "status", "state", "service", "conclusion") if any(k in r for r in recs[:50])]))
    head = [json.dumps(r, ensure_ascii=False)[:300] for r in recs[: int(rule.get("sample_lines", 15))]]
    note = (f"[chrono · intermediario] {len(recs)} registros ({len(text.encode()) // 1024} KB) en la salida de {tool}; "
            "resumen exacto:")
    return summary(rows, total_note=note, source_id=source_id, sample=head,
                   sample_note=f"primeros {len(head)} registros:", dims=dims)


# ── Read de ficheros grandes ──────────────────────────────────────────────────
def read_hook(cfg, inp, root):
    rcfg = cfg.get("read") or {}
    if not rcfg.get("enabled", True):
        return None
    path = inp.get("file_path", "")
    if not path or inp.get("offset") or inp.get("limit") or not os.path.isfile(path):
        return None  # lecturas parciales: siempre permitidas
    exts = rcfg.get("extensions", [".log", ".jsonl", ".ndjson", ".csv", ".tsv"])
    size = os.path.getsize(path)
    if size < int(rcfg.get("min_bytes", 200_000)) or not any(path.endswith(e) for e in exts):
        return None
    bin_ = chrono_bin()
    if not bin_:
        return None
    db = os.path.join(root, ".chrono", "index.db")
    srcs = subprocess.run([bin_, "sources", "--compact", "--db", db], capture_output=True, text=True, timeout=60).stdout if os.path.exists(db) else ""
    if path not in srcs:
        subprocess.run([bin_, "add", path, "--db", db], cwd=root, capture_output=True, text=True, timeout=300)
    q = lambda *a: subprocess.run([bin_, *a, "--compact", "--db", db], capture_output=True, text=True, timeout=60).stdout.strip()
    src = next((l.split("\t")[0] for l in q("sources").splitlines() if path in l or os.path.basename(path) in l), path)
    with open(path, errors="replace") as f:
        n_lines = sum(1 for _ in f)
    parts = [
        f"[chrono · intermediario] {path} pesa {size // 1024} KB ({n_lines} líneas): leerlo entero cuesta ~{size // 4} tokens "
        "y no sirve para contar. Está indexado en chrono; resumen exacto:",
        q("top", "level", "--source", src), q("top", "entity", "--source", src, "--limit", "15"),
        q("patterns", "--source", src, "--level", rcfg.get("levels", "FATAL,ERROR,CRITICAL,WARNING,WARN"), "--limit", "10"),
        f"Para cifras y para leer en orden: tools/CLI de chrono (top, patterns, timeline, events) con source={src} y filtros "
        "level, entity, attr, grep, since/until. Para ver un trozo literal: Read con offset y limit (eso no se intercepta).",
    ]
    return "\n".join(p for p in parts if p)


# ── Entrada ───────────────────────────────────────────────────────────────────
def main():
    if os.environ.get("CHRONO_INTERMEDIARIO", "1") == "0":
        return
    mode = sys.argv[1] if len(sys.argv) > 1 else "post"
    ev = json.load(sys.stdin)
    root = find_root(ev.get("cwd"))
    if not root:
        return
    cfg = load_config(root)
    if not cfg.get("enabled", True):
        return
    tool = ev.get("tool_name", "")
    inp = ev.get("tool_input") or {}
    t0 = time.time()
    if mode == "read":
        reason = read_hook(cfg, inp, root)
        if reason:
            log(f"read {inp.get('file_path')} → resumen ({len(reason)} c)")
            print(json.dumps({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny",
                                                     "permissionDecisionReason": reason}}, ensure_ascii=False))
        return
    for rule in cfg.get("rules", []):
        if not re.fullmatch(rule.get("tool", ""), tool):
            continue
        deadline = t0 + float(rule.get("timeout_secs", 45))
        if rule.get("adapter") == "loki":
            out = adapter_loki(rule, inp, root, deadline)
        elif rule.get("adapter") == "records":
            out = adapter_records(rule, tool, ev.get("tool_response"), root)
        else:
            out = None
        if out:
            log(f"post {tool} [{rule.get('adapter')}] → {len(out)} c en {time.time() - t0:.1f}s")
            print(json.dumps({"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedMCPToolOutput": out}},
                             ensure_ascii=False))
        return


if __name__ == "__main__":
    try:
        main()
    except Exception:
        log("ERROR " + traceback.format_exc().replace("\n", " | "))
    sys.exit(0)
