#!/usr/bin/env python3
"""Banco «chrono vs. sin chrono» para preguntas reales de programación.

Para cada escenario mide lo que un agente tendría que LEER para responder bien:

- sin chrono: la salida cruda de los comandos que haría falta (`git log …`,
  `gh run view --log-failed` × N, la API de jobs…). Es lo que acaba en su
  contexto si no hay índice.
- con chrono: la salida `--compact` de la consulta equivalente, y su latencia.
- alternativa hábil: cuando un pipeline de shell razonable (awk, sort | uniq -c)
  responde igual de corto, se mide también. Ahí chrono no gana en tokens y el
  informe lo dice.

Tokens ≈ bytes / 4 (aproximación; el orden de magnitud es lo que cuenta).

Uso:
  simular.py --repo RUTA_REPO --db RUTA_INDICE --gh OWNER/REPO [--chrono BIN]
             [--logs RUTA_JSONL] [--transcripts DIR] [--json salida.json]
"""

import argparse
import json
import os
import subprocess
import sys
import time


def run(cmd, cwd=None, shell=False):
    t0 = time.perf_counter()
    p = subprocess.run(cmd, cwd=cwd, shell=shell, capture_output=True, text=True, errors="replace")
    return p.stdout, (time.perf_counter() - t0) * 1000


def tok(n_bytes):
    return max(1, n_bytes // 4)


class Banco:
    def __init__(self, a):
        self.a = a
        self.filas = []

    def chrono(self, *args):
        out, ms = run([self.a.chrono, *args, "--db", self.a.db, "--compact"])
        return out, ms

    def fila(self, caso, pregunta, sin_bytes, sin_como, chrono_out, chrono_ms, chrono_como, habil=None, nota=""):
        c = len(chrono_out.encode())
        self.filas.append({
            "caso": caso,
            "pregunta": pregunta,
            "sin_chrono_tokens": tok(sin_bytes),
            "sin_chrono": sin_como,
            "chrono_tokens": tok(c),
            "chrono_ms": round(chrono_ms),
            "chrono": chrono_como,
            "habil_tokens": tok(habil[0]) if habil else None,
            "habil": habil[1] if habil else None,
            "reduccion": round(sin_bytes / max(c, 1), 1),
            "nota": nota,
        })
        print(f"  {caso}: {tok(sin_bytes):>9,} → {tok(c):>6,} tokens ({chrono_ms:.0f} ms)", file=sys.stderr)

    # ── Código (git) ──────────────────────────────────────────────────────────

    def hotspots(self):
        raw, _ = run(["git", "log", "--numstat", "--format=%H %an %ad"], cwd=self.a.repo)
        habil, _ = run("git log --numstat --format= | awk 'NF==3{c[$3]++} END{for(f in c) print c[f], f}' | sort -rn | head -25",
                       cwd=self.a.repo, shell=True)
        out, ms = self.chrono("hotspots", "--limit", "25")
        self.fila("hotspots", "¿Qué ficheros son más peligrosos de tocar?", len(raw.encode()), "git log --numstat (historia entera)",
                  out, ms, "chrono hotspots", (len(habil.encode()), "git log | awk | sort | head"),
                  "El pipeline hábil cuenta cambios pero no pondera tamaño ni filtra ruido (lockfiles, excluidos por config).")

    def coupling(self, fichero):
        shas, _ = run(["git", "log", "--format=%H", "--", fichero], cwd=self.a.repo)
        total = len(shas.encode())
        for sha in shas.split():
            o, _ = run(["git", "show", "--name-only", "--format=%s", sha], cwd=self.a.repo)
            total += len(o.encode())
        out, ms = self.chrono("coupling", fichero, "--limit", "15")
        self.fila("acoplamiento", f"¿Qué suele cambiar junto con {fichero}?", total,
                  f"git log -- fichero + git show --name-only × {len(shas.split())} commits", out, ms, "chrono coupling",
                  nota="Sin índice hay que abrir cada commit del fichero; el soporte/confianza hay que calcularlo a mano.")

    def owners(self, ruta):
        raw, _ = run(["git", "log", "--format=%an", "--", ruta], cwd=self.a.repo)
        habil, _ = run(f"git log --format=%an -- {ruta} | sort | uniq -c | sort -rn", cwd=self.a.repo, shell=True)
        out, ms = self.chrono("owners", ruta)
        self.fila("dueños", f"¿Quién conoce {ruta}? (bus factor)", len(raw.encode()), "git log --format=%an -- ruta",
                  out, ms, "chrono owners", (len(habil.encode()), "git log | sort | uniq -c"),
                  "Empate en tokens: aquí un uniq -c basta. chrono añade el bus factor ponderado por líneas.")

    def bugs(self):
        raw, _ = run(["git", "log", "-i", "-E", "--grep=fix|bug|error|falla|arregl|corrig|hotfix", "--stat"], cwd=self.a.repo)
        out, ms = self.chrono("bugs")
        self.fila("bugs", "¿Dónde se concentran los arreglos y de qué tipo son?", len(raw.encode()), "git log --grep=fix… --stat",
                  out, ms, "chrono bugs", nota="Las categorías (data-db, network…) no salen de un grep: son la taxonomía de chrono.")

    # ── CI/CD (fuentes de comando gh) ────────────────────────────────────────

    def ci_salud(self):
        raw, _ = run(["gh", "run", "list", "--repo", self.a.gh, "--limit", "500", "--json",
                      "databaseId,workflowName,displayTitle,headBranch,headSha,event,status,conclusion,createdAt,url"])
        j1, _ = run(["gh", "run", "list", "--repo", self.a.gh, "--limit", "500", "--json", "conclusion",
                     "--jq", "group_by(.conclusion)|map({c:.[0].conclusion,n:length})"])
        j2, _ = run(["gh", "run", "list", "--repo", self.a.gh, "--limit", "500", "--json", "conclusion,workflowName",
                     "--jq", '[.[]|select(.conclusion=="failure")]|group_by(.workflowName)|map({w:.[0].workflowName,n:length})|sort_by(-.n)'])
        out1, ms1 = self.chrono("top", "level", "--source", "cmd:gh-runs")
        out2, ms2 = self.chrono("top", "entity", "--source", "cmd:gh-runs", "--level", "FAILURE")
        self.fila("ci-salud", "¿Cuánto falla el CI/CD y qué workflow más?", len(raw.encode()), "gh run list --json (500 ejecuciones)",
                  out1 + out2, ms1 + ms2, "chrono top level + top entity --level FAILURE",
                  (len((j1 + j2).encode()), "gh run list --jq group_by (×2)"),
                  "Empate con un --jq bien escrito; chrono aporta poder cruzarlo después con commits y despliegues.")

    def ci_fallos(self, n=25):
        ids, _ = run(["gh", "run", "list", "--repo", self.a.gh, "--limit", str(n), "--status", "failure", "--json", "databaseId", "-q", ".[].databaseId"])
        total = 0
        for rid in ids.split():
            o, _ = run(["gh", "run", "view", rid, "--repo", self.a.gh, "--log-failed"])
            total += len(o.encode())
        out, ms = self.chrono("patterns", "--source", "cmd:gh-fallos", "--level", "ERROR", "--limit", "15")
        self.fila("ci-causas", f"¿Por qué fallaron las últimas {n} ejecuciones?", total, f"gh run view --log-failed × {len(ids.split())}",
                  out, ms, "chrono patterns --level ERROR",
                  nota="Lo que hace hoy un agente es tail/grep de UN log; aquí salen las causas de todos, agrupadas.")

    def ci_lentos(self):
        cache = os.path.join(os.path.dirname(self.a.db), "cmd", "gh-steps.cmd.jsonl")
        sin = os.path.getsize(cache) if os.path.exists(cache) else 0
        out, ms = self.chrono("stats", "duration_s", "--by", "entity", "--kind", "ci.step", "--limit", "10")
        self.fila("ci-lentos", "¿Qué jobs/pasos tardan más (p50/p95)?", sin, "API de jobs de 60 ejecuciones (ya reducida a NDJSON; la cruda es mayor)",
                  out, ms, "chrono stats duration_s --by entity",
                  nota="Cota inferior del lado sin chrono: el JSON de la API sin procesar pesa más.")

    def ci_despliegue(self):
        q, _ = run([self.a.chrono, "events", "--source", "cmd:gh-deploys", "--level", "FAILURE", "--limit", "1", "--db", self.a.db])
        ev = json.loads(q)["result"]["events"][0]
        rid = ev["id"].split(":")[-1]
        info, _ = run(["gh", "run", "view", rid, "--repo", self.a.gh, "--json", "headSha,createdAt,conclusion,jobs,displayTitle"])
        sha = json.loads(info).get("headSha", "")
        hecho = json.loads(info).get("createdAt", "")
        ventana, _ = run(["git", "log", "--stat", f"--since={hecho[:10]}", f"--until={hecho[:10]} 23:59"], cwd=self.a.repo)
        show, _ = run(["git", "show", "--stat", sha], cwd=self.a.repo)
        sin = len(info.encode()) + len(ventana.encode()) + len(show.encode())
        o1, m1 = self.chrono("trace", ev["id"])
        o2, m2 = self.chrono("correlate", ev["id"], "--kind", "commit", "--delta", "2h", "--limit", "5")
        self.fila("despliegue", "Este despliegue falló: ¿qué commit lo rompió y cuál lo arregló?", sin,
                  "gh run view --json + git show + git log --stat del día", o1 + o2, m1 + m2, "chrono trace + correlate --kind commit",
                  nota="La unión despliegue ↔ commit ↔ ejecuciones de CI sale de los enlaces commit=sha.")

    # ── Datos masivos ─────────────────────────────────────────────────────────

    def logs(self):
        if not self.a.logs:
            return
        raw = os.path.getsize(self.a.logs)
        db = self.a.db + ".logs.db"
        for s in ("", "-wal", "-shm"):
            if os.path.exists(db + s):
                os.remove(db + s)
        run([self.a.chrono, "init", self.a.repo, "--db", db])
        run([self.a.chrono, "add", self.a.logs, "--db", db])
        t0 = time.perf_counter()
        o1 = subprocess.run([self.a.chrono, "top", "level", "--db", db, "--compact", "--source", "jsonl"], capture_output=True, text=True).stdout
        o2 = subprocess.run([self.a.chrono, "patterns", "--level", "ERROR", "--db", db, "--compact", "--limit", "15"], capture_output=True, text=True).stdout
        o3 = subprocess.run([self.a.chrono, "patterns", "--level", "WARNING", "--db", db, "--compact", "--limit", "15"], capture_output=True, text=True).stdout
        ms = (time.perf_counter() - t0) * 1000
        self.fila("logs-24h", "Logs de 24 h de un entorno: ¿qué está fallando?", raw, "las 24 h de logs (Loki/Grafana) en crudo",
                  o1 + o2 + o3, ms, "chrono top level + patterns --level ERROR/WARNING",
                  nota="Medido con las plantillas nuevas (hex, ip, fechas dentro de tokens).")

    def tabla(self):
        print("| Caso | Pregunta | Sin chrono (tokens) | Con chrono (tokens) | Reducción | Latencia chrono | Alternativa hábil |")
        print("|---|---|---:|---:|---:|---:|---|")
        for f in self.filas:
            hab = f"{f['habil_tokens']:,} tok ({f['habil']})" if f["habil"] else "—"
            print(f"| {f['caso']} | {f['pregunta']} | {f['sin_chrono_tokens']:,} | {f['chrono_tokens']:,} | ×{f['reduccion']:,} | {f['chrono_ms']} ms | {hab} |")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", required=True)
    ap.add_argument("--db", required=True)
    ap.add_argument("--gh", required=True)
    ap.add_argument("--chrono", default="chrono")
    ap.add_argument("--logs")
    ap.add_argument("--fichero", default="backend/api_gateway/src/main.py")
    ap.add_argument("--ruta", default="backend/messaging_service")
    ap.add_argument("--json")
    a = ap.parse_args()
    b = Banco(a)
    for paso in (b.hotspots, lambda: b.coupling(a.fichero), lambda: b.owners(a.ruta), b.bugs,
                 b.ci_salud, b.ci_fallos, b.ci_lentos, b.ci_despliegue, b.logs):
        paso()
    b.tabla()
    if a.json:
        json.dump(b.filas, open(a.json, "w"), ensure_ascii=False, indent=2)


if __name__ == "__main__":
    main()
