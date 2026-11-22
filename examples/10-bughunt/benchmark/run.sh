#!/usr/bin/env bash
# Run the bughunt benchmark: one lab per gold set, one hunt each, scored.
#   benchmark/run.sh REPO OUT_DIR SUMMONR_BIN
# Fix-commit gold sets run at the fix's parent; the Gemini set at 5fb6bbe.
# Prints one summary line per gold set; full results in OUT_DIR/bench-*.json.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$1"; out="$(mkdir -p "$2" && cd "$2" && pwd)"; bin="$3"
# The rite's three default angles.
angles='["ways to bypass a permission, scope or resource limit", "panics, overflows or unbounded work on hostile input", "injection: untrusted text reaching a path, command, header, query or prompt"]'

bench() {  # name commit gold dirs
  local name="$1" commit="$2" gold="$3" dirs="$4" lab="$out/$1"
  "$here/../setup.sh" "$lab" "$repo" "$commit" "$bin" >/dev/null || { echo "$name setup failed"; return; }
  local s=$SECONDS
  SUMMONR_HOME="$lab/home" "$bin" run bughunt-bench --arg "dirs=$dirs" --arg "angles=$angles" \
    --arg "gold=$gold" --wait --json --timeout 3h > "$out/bench-$name.json" 2>&1
  local code=$? took=$((SECONDS - s))
  python3 -I - "$out/bench-$name.json" "$name" "$code" "$took" <<'EOF'
import json, sys
path, name, code, took = sys.argv[1:]
try:
    d = json.load(open(path))["data"]
except Exception as e:
    print(f"{name}: exit {code}, unreadable result ({e})"); sys.exit()
r = d.get("result") or {}
raw, kept = r.get("raw") or {}, r.get("after_gather") or {}
print(f"{name}: {d.get('outcome')} exit {code} {took}s requests {d.get('requests')} tokens {d.get('tokens')} | "
      f"gold {raw.get('gold_total')} | raw: {raw.get('findings')} findings, {raw.get('hits')} hits, gold hit {raw.get('gold_hit')} | "
      f"gathered: {kept.get('findings')} findings, {kept.get('hits')} hits, gold hit {kept.get('gold_hit')} {kept.get('hit_ids')}"
      + (f" | error: {d.get('error')}" if d.get('error') else ""))
EOF
  SUMMONR_HOME="$lab/home" "$bin" core stop --json >/dev/null 2>&1 || true
}

parent() { git -C "$repo" rev-parse --short "$1^"; }
# Directories are the ones holding each gold set's files.
bench fix-c06fd86 "$(parent c06fd86)" fix-c06fd86.json '["src"]'
bench fix-4979f5c "$(parent 4979f5c)" fix-4979f5c.json '["src","src/reagents"]'
bench fix-35f37f0 "$(parent 35f37f0)" fix-35f37f0.json '["src"]'
bench gemini 5fb6bbe gemini-125.json '["src/reagents"]'
