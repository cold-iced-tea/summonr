#!/usr/bin/env python3
"""One summary line for a bench result: summary.py BENCH_JSON NAME SECONDS."""
import json
import sys

path, name, took = sys.argv[1:]
try:
    d = json.load(open(path))
except Exception as e:  # an empty file means the bench row was missing
    print(f"{name}: unreadable result ({e})")
    sys.exit()
r = d.get("result") or {}
raw, kept = r.get("raw") or {}, r.get("after_gather") or {}
line = (
    f"{name}: {d.get('outcome')} {took}s requests {d.get('requests')} tokens {d.get('tokens')} | "
    f"gold {raw.get('gold_total')} | raw: {raw.get('findings')} findings, {raw.get('hits')} hits, "
    f"gold hit {raw.get('gold_hit')} | gathered: {kept.get('findings')} findings, {kept.get('hits')} hits, "
    f"gold hit {kept.get('gold_hit')} {kept.get('hit_ids')}"
)
if r.get("hunts_failed"):
    line += f" | hunts failed {r['hunts_failed']}"
if d.get("error"):
    line += f" | error: {d['error']}"
print(line)
