#!/usr/bin/env bash
# Run every bughunt drill offline in a throwaway home (including the sandboxed
# write-path drill for bughunt-test).
#   examples/10-bughunt/drill.sh [path/to/summonr]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
bin="${1:-summonr}"
home="$(mktemp -d /tmp/summonr-bughunt-drill-XXXXXX)"
export SUMMONR_HOME="$home"
trap '"$bin" core stop --json >/dev/null 2>&1 || true; rm -rf "$home"' EXIT

# Turn a copy of fixtures/target into a tiny git repo with a local bare remote
# so bughunt-test's sandboxed drill can exercise real worktree + write + trial.
repo="$home/target"
cp -a "$here/fixtures/target" "$repo"
: > "$repo/src/bughunt_repro.rs"
git -C "$repo" init -q -b bughunt-base
git -C "$repo" config user.name bughunt
git -C "$repo" config user.email bughunt@localhost
git -C "$repo" add .
git -C "$repo" commit -q -m "fixture base"
git init -q --bare "$home/remote.git"
git -C "$repo" remote add lab "$home/remote.git"
git -C "$repo" push -q lab bughunt-base

# Lightweight deterministic trial runner for test.drill.yaml (no cargo build).
cat > "$home/fake-trial.py" <<'PY'
import pathlib, sys
src = pathlib.Path("src/bughunt_repro.rs").read_text()
if "PASS_CASE" in src:
    print("running 1 test\n.\ntest result: ok. 1 passed; 0 failed; 0 ignored")
    sys.exit(0)
if "BUGHUNT-CONFIRMED" in src:
    print("thread 'bughunt_repro::repro' panicked:\nBUGHUNT-CONFIRMED: simulated failure\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored")
    sys.exit(1)
print("error: could not compile `fixture` due to 1 previous error")
sys.exit(101)
PY

mkdir -p "$home/config" "$home/rituals"
cat > "$home/config/config.yaml" <<EOF
repos:
  bughunt-target:
    path: $repo
    trunk: bughunt-base
    remote: lab
    test: python3 $home/fake-trial.py
    disposable: [target/]
EOF

for f in "$here"/*.yaml; do
  base="$(basename "$f")"
  case "$base" in *.drill.yaml|*.rite.yaml|*.assay.yaml) continue ;; esac
  sed -e "s|/absolute/path/to/lab|$here/fixtures|g" \
      -e "s|/absolute/path/to/target-repro|$repo-repro|g" \
      -e "s|/absolute/path/to/target|$repo|g" "$f" > "$home/rituals/$base"
done
"$bin" inscribe "$home/rituals" --json > "$home/inscribe.json" \
  || { echo "inscribe failed" >&2; cat "$home/inscribe.json" >&2; exit 1; }

# Verify zero compile-time lint warnings across all inscribed rituals.
python3 - "$home/inscribe.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
warns = list(d.get("warnings") or [])
for item in (d.get("data") or {}).get("inscribed") or []:
    warns.extend(item.get("warnings") or [])
if warns:
    print("WARNINGS:", warns, file=sys.stderr)
    sys.exit(1)
PY

"$here/grant.sh" "$bin" "$repo" "$here/fixtures"

status=0
for d in "$here"/*.drill.yaml; do
  name="$(sed -n 's/^ritual: //p' "$d")"
  if "$bin" drill "$name" against "$d" --json > "$home/out.json"; then
    echo "PASS $(basename "$d")"
  else
    echo "FAIL $(basename "$d")"; cat "$home/out.json"; status=1
  fi
done
exit $status
