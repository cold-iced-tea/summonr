#!/usr/bin/env bash
# Run the bughunt benchmark: one lab per gold set, the rite's three angles.
#   benchmark/run.sh REPO OUT_DIR SUMMONR_BIN [--tier smoke|core|full] [--category CAT]
#                    [--suite SUITE_JSON] [--gold-dir DIR] [NAME...]
#
# Reads cases from benchmark/suite.json, or --suite (default: --tier smoke
# unless NAMEs or --category are given). For external-suite.json, REPO is the
# fetch_external.py cache (one clone per repo_name, each case's crate_path
# passed to setup.sh) and --gold-dir the gold sets it built there. Fix-commit gold sets run at the fix's parent commit;
# audit sets run at their target_commit.
#
# Prints a live workbench attach hint for each lab, one summary line per gold
# set, and a category rollup table at the end. Results: OUT_DIR/bench-NAME.json
# and OUT_DIR/hunts-NAME.json (raw hunts, for bughunt-rescore). Needs sqlite3.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$1"; out="$(mkdir -p "$2" && cd "$2" && pwd)"; bin="$3"; shift 3

tier=""
category=""
suite="$here/suite.json"
gold_dir=""
only=()
while [ $# -gt 0 ]; do
  case "$1" in
    --tier) tier="$2"; shift 2 ;;
    --category) category="$2"; shift 2 ;;
    --suite) suite="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"; shift 2 ;;
    --gold-dir) gold_dir="$(cd "$2" && pwd)"; shift 2 ;;
    *) only+=("$1"); shift ;;
  esac
done

angles='["ways to bypass a permission, scope or resource limit", "panics, overflows or unbounded work on hostile input", "injection: untrusted text reaching a path, command, header, query or prompt"]'

summary() {  # name took
  python3 -I "$here/summary.py" "$out/bench-$1.json" "$1" "$2"
}

bench() {  # name commit gold dirs case_repo crate_path
  local name="$1" commit="$2" gold="$3" dirs="$4" case_repo="$5" crate_path="$6" lab="$out/$1"
  "$here/../setup.sh" "$lab" "$case_repo" "$commit" "$bin" "$crate_path" >/dev/null || { echo "$name: setup failed"; return; }
  if [ -n "$gold_dir" ]; then
    [ -f "$gold_dir/$gold" ] || { echo "$name: no $gold in $gold_dir"; return; }
    cp "$gold_dir/$gold" "$lab/benchmark/"
  fi
  echo "[$name] watch live: SUMMONR_HOME=\"$lab/home\" $bin console"
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

# Resolve selected cases from suite.json.
mapfile -t selected < <(python3 -I - "$suite" "$repo" "$tier" "$category" "${only[@]}" <<'PY'
import json, os, subprocess, sys

suite_path, repo, tier, category, *only = sys.argv[1:]
suite = json.load(open(suite_path, encoding="utf-8"))
tier_rank = {"smoke": 0, "core": 1, "full": 2}
if not tier and not category and not only:
    tier = "smoke"
max_rank = tier_rank.get(tier, 2) if tier else 2

for c in suite.get("cases", []):
    if only and c["name"] not in only:
        continue
    if category and c.get("category") != category:
        continue
    if tier and tier_rank.get(c.get("tier", "full"), 2) > max_rank:
        continue
    case_repo = os.path.join(repo, c["repo_name"]) if c.get("repo_name") else repo
    if c.get("fix_commit"):
        commit = subprocess.check_output(
            ["git", "-C", case_repo, "rev-parse", "--short", f"{c['fix_commit']}^"],
            text=True,
        ).strip()
    else:
        commit = c["target_commit"]
    dirs = json.dumps(c["dirs"])
    # crate_path last: an empty trailing field survives `read`.
    print(f"{c['name']}\t{commit}\t{c['gold']}\t{dirs}\t{case_repo}\t{c.get('crate_path', '')}")
PY
)

if [ ${#selected[@]} -eq 0 ]; then
  echo "No matching cases in $suite" >&2
  exit 1
fi

for row in "${selected[@]}"; do
  IFS=$'\t' read -r name commit gold dirs case_repo crate_path <<< "$row"
  bench "$name" "$commit" "$gold" "$dirs" "$case_repo" "$crate_path"
done

echo ""
python3 -I "$here/report.py" "$out" --suite "$suite"
