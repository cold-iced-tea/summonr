#!/usr/bin/env bash
# Run the bughunt benchmark: one lab per gold set, the rite's three angles.
#   benchmark/run.sh REPO OUT_DIR SUMMONR_BIN [NAME...]
# Fix-commit gold sets run at the fix's parent; the Gemini set at 5fb6bbe.
# With NAMEs (e.g. fix-c06fd86 gemini-reagents), runs only those.
# Prints one summary line per gold set. Results: OUT_DIR/bench-NAME.json (the
# bench's outcome and result) and OUT_DIR/hunts-NAME.json (the raw hunts, for
# bughunt-rescore). Needs sqlite3.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$1"; out="$(mkdir -p "$2" && cd "$2" && pwd)"; bin="$3"; shift 3
only=("$@")
angles='["ways to bypass a permission, scope or resource limit", "panics, overflows or unbounded work on hostile input", "injection: untrusted text reaching a path, command, header, query or prompt"]'

summary() {  # name took
  python3 -I "$here/summary.py" "$out/bench-$1.json" "$1" "$2"
}

bench() {  # name commit gold dirs
  local name="$1" commit="$2" gold="$3" dirs="$4" lab="$out/$1"
  if [ ${#only[@]} -gt 0 ] && [[ ! " ${only[*]} " == *" $name "* ]]; then return; fi
  "$here/../setup.sh" "$lab" "$repo" "$commit" "$bin" >/dev/null || { echo "$name: setup failed"; return; }
  local s=$SECONDS
  # --brief avoids oversized RPC frames; --cancel-on-drop stops the summon if
  # this script is killed. Results are also extracted from SQLite for hunts-*.
  SUMMONR_HOME="$lab/home" "$bin" run bughunt-bench --arg "dirs=$dirs" --arg "angles=$angles" \
    --arg "gold=$gold" --wait --brief --cancel-on-drop --json --timeout 4h > /dev/null 2>&1
  local took=$((SECONDS - s)) db="$lab/home/state/summonr.db"
  sqlite3 "$db" "select json_object('outcome', outcome, 'error', error, 'requests', requests, 'tokens', tokens, 'result', json(result)) from summons where ritual='bughunt-bench' order by started desc limit 1" > "$out/bench-$name.json"
  sqlite3 "$db" "select '[' || coalesce(group_concat(result, ','), '') || ']' from summons where ritual='bughunt-hunt' and outcome='returned'" > "$out/hunts-$name.json"
  summary "$name" "$took"
  SUMMONR_HOME="$lab/home" "$bin" core stop --json >/dev/null 2>&1 || true
}

parent() { git -C "$repo" rev-parse --short "$1^"; }
# Directories are the ones holding each gold set's files.
bench fix-c06fd86 "$(parent c06fd86)" fix-c06fd86.json '["src"]'
bench fix-4979f5c "$(parent 4979f5c)" fix-4979f5c.json '["src","src/reagents"]'
bench fix-35f37f0 "$(parent 35f37f0)" fix-35f37f0.json '["src"]'
bench gemini-reagents 5fb6bbe gemini-reagents.json '["src/reagents"]'
