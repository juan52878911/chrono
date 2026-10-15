#!/usr/bin/env python3
"""Anonimiza un volcado JSONL de logs para el corpus del bench, de forma DETERMINISTA
(la misma entrada da los mismos bytes): IPv4 públicas → 203.0.113.0/24 (rango de
documentación), UUID → otro UUID derivado por hash, dominios propios → example.test.
Las IP privadas (10/8, 172.16/12, 192.168/16) se quedan: no identifican a nadie.
Uso: anonimizar.py entrada.jsonl > salida.jsonl"""
import hashlib, ipaddress, json, re, sys

def h(s, n=8):
    return hashlib.sha256(s.encode()).hexdigest()[:n]

def ip(m):
    a = m.group(0)
    try:
        if ipaddress.ip_address(a).is_private:
            return a
    except ValueError:
        return a
    return f"203.0.113.{int(h(a, 4), 16) % 254 + 1}"

def uuid(m):
    x = h(m.group(0), 32)
    return f"{x[:8]}-{x[8:12]}-{x[12:16]}-{x[16:20]}-{x[20:]}"

UUID = re.compile(r"\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
IPV4 = re.compile(r"\b\d{1,3}(?:\.\d{1,3}){3}\b")
DOM = [(re.compile(r"aura\.asccilabs\.com"), "crm.example.test"), (re.compile(r"asccilabs"), "example")]

def clean(s):
    s = UUID.sub(uuid, s)
    s = IPV4.sub(ip, s)
    for r, rep in DOM:
        s = r.sub(rep, s)
    return s

for line in sys.stdin if len(sys.argv) < 2 else open(sys.argv[1]):
    r = json.loads(line)
    print(json.dumps({k: clean(v) if isinstance(v, str) else v for k, v in r.items()}, ensure_ascii=False, sort_keys=True))
