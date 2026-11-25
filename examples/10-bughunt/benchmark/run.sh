#!/usr/bin/env bash
# Run the bughunt benchmark over a suite of gold sets, the rite's three angles.
#   benchmark/run.sh REPO OUT_DIR SUMMONR_BIN [--isolated | --shared-core | --core-home PATH]
#                    [--tier smoke|core|full] [--category CAT] [--suite SUITE_JSON]
#                    [--gold-dir DIR] [--model M] [NAME...]
#
# Reads cases from benchmark/suite.json, or --suite (default: --tier smoke
# unless NAMEs or --category are given). For external-suite.json, REPO is the
# fetch_external.py cache (one clone per repo_name) and --gold-dir the gold
# sets it built there. Fix-commit gold sets run at the fix's parent commit;
# audit sets run at their target_commit. A `"lang": "python"` case
# (python-suite.json) hunts .py files with no test-module marker (ext=.py,
# stop_at="") and its isolated lab is set up with --lang python.
#
# Two ways to run:
#   --isolated (default)  One standalone lab per case (setup.sh): its own clone,
#                         SUMMONR_HOME and core, so a run is reproducible on
#                         its own and leaves your core alone.
#   --shared-core         Every case on the core you already use (SUMMONR_HOME,
#                         or the default home). The bench rituals are saved once
#                         (a new version only if a file changed), granted with
#                         --under OUT_DIR and --under the gold directory, and
#                         each case's parent commit is unpacked read-only into
#                         OUT_DIR/NAME/target and passed as `root`.
#   --core-home PATH      --shared-core on the core whose home is PATH.
#
# Prints a live console hint per case, one summary line per gold set, and a
# category rollup at the end. Results: OUT_DIR/bench-NAME.json and
# OUT_DIR/hunts-NAME.json (raw hunts, for bughunt-rescore), taken from the
# run's own --json record.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
lab_src="$(cd "$here/.." && pwd)"
repo="$1"; out="$(mkdir -p "$2" && cd "$2" && pwd)"; bin="$3"; shift 3

mode=isolated
tier=""
category=""
suite="$here/suite.json"
gold_dir=""
model=()
only=()
while [ $# -gt 0 ]; do
  case "$1" in
    --isolated) mode=isolated; shift ;;
    --shared-core) mode=shared; shift ;;
    --core-home) mode=shared; export SUMMONR_HOME="$(mkdir -p "$2" && cd "$2" && pwd)"; shift 2 ;;
    --tier) tier="$2"; shift 2 ;;
    --category) category="$2"; shift 2 ;;
    --suite) suite="$(cd "$(dirname "$2")" && pwd)/$(basename "$2")"; shift 2 ;;
    --gold-dir) gold_dir="$(cd "$2" && pwd)"; shift 2 ;;
    --model) model=(--model "$2"); shift 2 ;;
    *) only+=("$1"); shift ;;
  esac
done

angles='["ways to bypass a permission, scope or resource limit", "panics, overflows or unbounded work on hostile input", "injection: untrusted text reaching a path, command, header, query or prompt"]'

summary() {  # name took
  python3 -I "$here/summary.py" "$out/bench-$1.json" "$1" "$2"
}

# bench-NAME.json and hunts-NAME.json from `run --wait --brief --json`. The
# brief record keeps the whole result; when the bench did not return, the
# hunts that did are read from its child runs.
extract() {  # name run_json [SUMMONR_HOME]
  ( if [ -n "${3:-}" ]; then export SUMMONR_HOME="$3"; fi
  python3 -I - "$1" "$2" "$out" "$bin" <<'PY'
import json, os, subprocess, sys

name, run_json, out, bin_ = sys.argv[1:]
try:
    env = json.load(open(run_json, encoding="utf-8"))
except (OSError, ValueError) as e:
    env = {"error": {"message": f"no readable run output ({e})"}}
d = env.get("data") or {}
err = d.get("error") or (env.get("error") or {}).get("message")
bench = {"outcome": d.get("outcome"), "error": err, "requests": d.get("requests"),
         "tokens": d.get("tokens"), "result": d.get("result")}
hunts = (d.get("result") or {}).get("hunts")
if hunts is None:
    hunts = []
    for c in d.get("children") or []:
        if c.get("ritual") != "bughunt-hunt" or c.get("outcome") != "returned":
            continue
        p = subprocess.run([bin_, "record", c["id"], "--brief", "--json"], capture_output=True, text=True)
        try:
            r = (json.loads(p.stdout).get("data") or {}).get("result")
        except ValueError:
            r = None
        if r is not None:
            hunts.append(r)
with open(os.path.join(out, f"bench-{name}.json"), "w", encoding="utf-8") as f:
    json.dump(bench, f)
    f.write("\n")
with open(os.path.join(out, f"hunts-{name}.json"), "w", encoding="utf-8") as f:
    json.dump(hunts, f)
    f.write("\n")
PY
  )
}

run_bench() {  # name dirs gold root benchmark_root lang  (SUMMONR_HOME already set)
  local name="$1" dirs="$2" gold="$3" root="$4" broot="$5" lang="$6" by_lang=()
  [ "$lang" = python ] && by_lang=(--arg ext=.py --arg stop_at=)
  # --brief keeps the reply small but the result whole; --cancel-on-drop
  # stops the summon if this script is killed or stops waiting.
  "$bin" run bughunt-bench --arg "root=$root" --arg "benchmark_root=$broot" \
    --arg "dirs=$dirs" --arg "angles=$angles" --arg "gold=$gold" "${by_lang[@]}" "${model[@]}" \
    --wait --brief --cancel-on-drop --json --timeout 4h > "$out/run-$name.json" 2>"$out/run-$name.err"
}

isolated_bench() {  # name commit gold dirs case_repo lang path
  local name="$1" commit="$2" gold="$3" dirs="$4" case_repo="$5" lang="$6" path="$7" lab="$out/$1"
  "$here/../setup.sh" "$lab" "$case_repo" "$commit" "$bin" "$path" --lang "$lang" >/dev/null || { echo "$name: setup failed"; return; }
  if [ -n "$gold_dir" ]; then
    [ -f "$gold_dir/$gold" ] || { echo "$name: no $gold in $gold_dir"; return; }
    cp "$gold_dir/$gold" "$lab/benchmark/"
  fi
  echo "[$name] watch live: SUMMONR_HOME=\"$lab/home\" $bin console"
  local s=$SECONDS
  SUMMONR_HOME="$lab/home" run_bench "$name" "$dirs" "$gold" "$lab/target" "$lab/benchmark" "$lang"
  extract "$name" "$out/run-$name.json" "$lab/home"
  summary "$name" "$((SECONDS - s))"
  SUMMONR_HOME="$lab/home" "$bin" core stop --json >/dev/null 2>&1 || true
}

# Shared core: the gold sets are read where they are.
broot="${gold_dir:-$here}"

shared_setup() {
  # Children before parents; identical contents keep their version.
  local r
  for r in scan-file hunt gather score bench; do
    "$bin" inscribe "$lab_src/$r.yaml" --replace --json > "$out/inscribe-$r.json" \
      || { echo "inscribe $r.yaml failed:" >&2; cat "$out/inscribe-$r.json" >&2; exit 1; }
  done
  allow() { "$bin" allow "$@" --json >/dev/null || { echo "grant failed: $*" >&2; exit 1; }; }
  allow bughunt-scan-file --from file --select read_text --under "$out"
  allow bughunt-hunt --from file --select walk,read_text --under "$out"
  allow bughunt-bench --from file --select walk,read_text --under "$out"
  allow bughunt-bench --from file --select read_json --under "$broot"
}

shared_bench() {  # name commit gold dirs case_repo lang path
  local name="$1" commit="$2" gold="$3" dirs="$4" case_repo="$5" lang="$6" case_dir="$out/$1"
  [ -f "$broot/$gold" ] || { echo "$name: no $gold in $broot"; return; }
  local target="$case_dir/target" want
  want="$(git -C "$case_repo" rev-parse "$commit^{commit}")" || { echo "$name: no commit $commit in $case_repo"; return; }
  if [ "$(cat "$case_dir/COMMIT" 2>/dev/null)" != "$want" ]; then
    [ -d "$target" ] && { chmod -R u+w "$target"; rm -rf "$target"; }
    mkdir -p "$target"
    git -C "$case_repo" archive "$want" | tar -x -C "$target" || { echo "$name: checkout failed"; return; }
    # The hunt reads files and refuses symlinks; a snapshot needs none.
    find "$target" -type l -delete
    chmod -R a-w "$target"
    echo "$want" > "$case_dir/COMMIT"
  fi
  echo "[$name] watch live: $bin console"
  local s=$SECONDS
  run_bench "$name" "$dirs" "$gold" "$target" "$broot" "$lang"
  extract "$name" "$out/run-$name.json"
  summary "$name" "$((SECONDS - s))"
}

# Resolve selected cases from the suite.
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
    lang = c.get("lang", "rust")
    # The crate (Rust) or import root (Python) last: an empty trailing field
    # survives `read`.
    path = c.get("import_root", "") if lang == "python" else c.get("crate_path", "")
    print(f"{c['name']}\t{commit}\t{c['gold']}\t{dirs}\t{case_repo}\t{lang}\t{path}")
PY
)

if [ ${#selected[@]} -eq 0 ]; then
  echo "No matching cases in $suite" >&2
  exit 1
fi

if [ "$mode" = shared ]; then
  echo "shared core: ${SUMMONR_HOME:-default home}; granting --under $out and --under $broot"
  shared_setup
fi

for row in "${selected[@]}"; do
  IFS=$'\t' read -r name commit gold dirs case_repo lang path <<< "$row"
  "${mode}_bench" "$name" "$commit" "$gold" "$dirs" "$case_repo" "$lang" "$path"
done

echo ""
python3 -I "$here/report.py" "$out" --suite "$suite"
if [ "$mode" = shared ]; then
  echo ""
  echo "The --under grants stay on the core; to withdraw them:"
  for r in scan-file:read_text hunt:walk,read_text bench:walk,read_text; do
    echo "  $bin disallow bughunt-${r%%:*} --from file --select ${r#*:} --under $out"
  done
  echo "  $bin disallow bughunt-bench --from file --select read_json --under $broot"
fi
