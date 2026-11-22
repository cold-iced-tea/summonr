#!/usr/bin/env bash
# Run every bughunt drill offline in a throwaway home.
# Rituals name literal roots, so each one is rewritten to point at
# fixtures/target before it is inscribed (see LIMITS.md, "fixed roots").
#   examples/10-bughunt/drill.sh [path/to/summonr]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
bin="${1:-summonr}"
home="$(mktemp -d /tmp/summonr-bughunt-drill-XXXXXX)"
export SUMMONR_HOME="$home"
trap '"$bin" core stop --json >/dev/null 2>&1 || true; rm -rf "$home"' EXIT

fixture="$here/fixtures/target"
# Children before parents: a ritual step needs its child already inscribed.
for name in verdict batch-ready gather hunt test score bench; do
  f="$here/$name.yaml"; [ -f "$f" ] || continue
  sed -e "s|/absolute/path/to/lab|$here/fixtures|g" \
      -e "s|/absolute/path/to/target-repro|$fixture-repro|g" \
      -e "s|/absolute/path/to/target|$fixture|g" "$f" > "$home/$(basename "$f")"
  "$bin" inscribe "$home/$(basename "$f")" --json >/dev/null \
    || { echo "inscribe failed: $name" >&2; "$bin" inscribe "$SUMMONR_HOME/$(basename "$f")" --json >&2; exit 1; }
done

"$here/grant.sh" "$bin" "$fixture" "$here/fixtures"

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
