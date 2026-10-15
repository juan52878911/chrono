#!/usr/bin/env python3
"""Corrector automático del bench de agentes, contra oro.json (misma rúbrica que la
corrección manual: 4 preguntas de 1 punto). Es heurístico —busca las cifras y los
hechos del oro en el texto de la respuesta—; `--calibrar` lo compara con notas
puestas a mano para saber cuánto fiarse.

  corregir.py <dir-resultados>            tabla por sesión + media por brazo
  corregir.py <dir> --gate umbrales.json  además sale con 1 si un brazo baja de su mínimo
  corregir.py --calibrar calibracion.json compara con notas manuales {fichero.txt|.jsonl: nota}
"""
import glob, json, os, re, statistics, sys

HERE = os.path.dirname(os.path.abspath(globals().get("__file__", "corregir.py")))
ORO = json.load(open(os.path.join(HERE, "oro.json")))
QUALIF = re.compile(r"sin nivel|sin etiqueta|deducid|inferid|por texto|texto dice|sin label|unlabel", re.I)

def final_text(path):
    if path.endswith(".txt"):
        return open(path, errors="replace").read()
    res = None
    for l in open(path, errors="replace"):
        try:
            e = json.loads(l)
        except Exception:
            continue
        if e.get("type") == "result":
            res = e
    return (res or {}).get("result", "") or ""

def metrics(path):
    m = {"turnos": None, "seg": None, "tokens_in": None, "chrono": 0, "otras": 0}
    if not path.endswith(".jsonl"):
        return m
    for l in open(path, errors="replace"):
        try:
            e = json.loads(l)
        except Exception:
            continue
        if e.get("type") == "assistant":
            for c in e["message"].get("content", []):
                if c.get("type") == "tool_use":
                    m["chrono" if "chrono" in c["name"] else "otras"] += 1
        if e.get("type") == "result":
            u = e.get("usage", {})
            m.update(turnos=e.get("num_turns"), seg=(e.get("duration_ms") or 0) // 1000,
                     tokens_in=u.get("input_tokens", 0) + u.get("cache_creation_input_tokens", 0) + u.get("cache_read_input_tokens", 0))
    return m

def nums(line):
    return {int(x.replace(".", "").replace(",", "")) for x in re.findall(r"(?<![\w:.\-])\d{1,3}(?:[.,]\d{3})*(?![\w.\-%]|:\d)", line)}

def grade(text):
    t = re.sub(r"[*`]", "", text)
    lines = t.splitlines()
    wrong = {"FATAL": 79, "ERROR": 15, "WARNING": 28}
    p1, señales = 0.0, []
    for L, g in ORO["p1_niveles"].items():
        rel = [l for l in lines if re.search(rf"\b{L}\b", l, re.I)]
        tot = any(g["total"] in nums(l) for l in rel)
        bad = [l for l in rel if wrong.get(L) in nums(l) and not QUALIF.search(l)]
        svc = all(any(s in l and n in nums(l) for l in lines) for s, n in g["servicios"].items())
        if bad:
            señales.append(f"{L}={wrong[L]} sin distinguir")
        if tot and svc and not bad:
            p1 += 0.25
    keys = [k["clave"].lower() for k in ORO["p2_top3"]]
    want = [k["veces"] for k in ORO["p2_top3"]]
    hit = [any(k in l.lower() and n in nums(l) for l in lines) for k, n in zip(keys, want)]
    pos = [t.lower().find(k) for k in keys]
    p2 = 1.0 if all(hit) else (0.5 if all(p >= 0 for p in pos) and pos == sorted(pos) else 0.0)
    if re.search(r"\b42\b.*\b18\b|\b42\b[^\n]{0,300}\n[^\n]*\b18\b", t):
        señales.append("top3 contado por texto (42/18/12)")
    p3 = (0.5 if re.search(r"DELETE\s+/api/collections", t) else 0) + (0.5 if "import_source_id" in t and "AttributeError" in t else 0)
    def has_hours(hs):
        found = set(re.findall(r"\b(\d{2}):(\d{2})", t))
        mins = {int(h) * 60 + int(m) for h, m in found}
        return all(any(abs(int(h[:2]) * 60 + int(h[3:]) - x) <= 1 for x in mins) for h in hs)
    # P4 se busca a partir de donde se habla de reinicios.
    i4 = min([i for i in (t.lower().find("reinici"), t.lower().find("restart")) if i >= 0] or [0])
    t4 = t[i4:]
    n = ORO["p4"]["reinicios"]
    claims = {int(x) for x in re.findall(r"\b(\d{1,2})\s+(?:reinicios|veces|ciclos|eventos de (?:apagado|reinicio)|restarts|apagados)", t4, re.I)}
    wrong_count = bool(claims) and not claims & {n, 2 * n}
    if wrong_count:
        señales.append(f"cuenta {sorted(claims)} reinicios")
    p4 = 0.5 if (has_hours(ORO["p4"]["horas_utc"]) or has_hours(ORO["p4"]["preflight_horas_utc"])) and not wrong_count else 0
    if "catalog version mismatch" in t4.lower():
        neg = re.search(r"(no hay|ningún|ninguna|sin)\s+(?:\w+\s+){0,2}(error|precursor|warning|aviso)|\bninguno\b|no (?:aparece|hay) (?:ningún )?error", t4, re.I)
        p4 += 0.25 if neg else 0.5
    return {"P1": p1, "P2": p2, "P3": p3, "P4": p4, "total": p1 + p2 + p3 + p4, "señales": señales}

def table(rows):
    print("sesión\tP1\tP2\tP3\tP4\ttotal\tchrono\totras\tturnos\tseg\ttokens_in\tseñales")
    for name, g, m in rows:
        print(f"{name}\t{g['P1']}\t{g['P2']}\t{g['P3']}\t{g['P4']}\t{g['total']}\t{m['chrono']}\t{m['otras']}\t{m['turnos']}\t{m['seg']}\t{m['tokens_in']}\t{'; '.join(g['señales'])}")

def main():
    if sys.argv[1] == "--calibrar":
        cal = json.load(open(sys.argv[2]))
        base = os.path.dirname(os.path.abspath(sys.argv[2]))
        errs = []
        for f, manual in cal.items():
            g = grade(final_text(f if os.path.isabs(f) else os.path.join(base, f)))
            errs.append(abs(g["total"] - manual))
            print(f"{os.path.basename(f)}\tmanual={manual}\tauto={g['total']}\tΔ={g['total'] - manual:+.2f}")
        print(f"\nerror absoluto medio {statistics.mean(errs):.2f} · máximo {max(errs):.2f} · n={len(errs)}")
        return 0
    d = sys.argv[1]
    rows = []
    for p in sorted(glob.glob(os.path.join(d, "*-[0-9]*.jsonl"))):
        name = os.path.basename(p)[:-6]
        rows.append((name, grade(final_text(p)), metrics(p)))
    table(rows)
    arms = {}
    for name, g, m in rows:
        arms.setdefault(name.rsplit("-", 1)[0], []).append((g, m))
    print("\nbrazo\tn\tnota_media\tdesv\tseñales\ttokens_in_mediana\tseg_mediana")
    summary = {}
    for a, v in sorted(arms.items()):
        tot = [g["total"] for g, _ in v]
        tok = [m["tokens_in"] for _, m in v if m["tokens_in"]]
        seg = [m["seg"] for _, m in v if m["seg"]]
        summary[a] = statistics.mean(tot)
        print(f"{a}\t{len(v)}\t{statistics.mean(tot):.2f}\t{statistics.pstdev(tot):.2f}\t{sum(len(g['señales']) for g, _ in v)}\t"
              f"{int(statistics.median(tok)) if tok else '-'}\t{int(statistics.median(seg)) if seg else '-'}")
    if "--gate" in sys.argv:
        th = json.load(open(sys.argv[sys.argv.index("--gate") + 1]))
        bad = [a for a, mn in th.items() if summary.get(a, 0) < mn]
        if bad:
            print(f"\nGATE: bajan de su mínimo: {', '.join(bad)}")
            return 1
    return 0

if __name__ == "__main__":
    sys.exit(main())
