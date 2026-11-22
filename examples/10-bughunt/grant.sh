#!/usr/bin/env bash
# Grant the bughunt rituals exactly the authority they declare.
#   grant.sh SUMMONR_BIN TARGET_ROOT
set -euo pipefail
bin="$1"; root="$2"
grant() { "$bin" allow "$@" --json >/dev/null || { echo "grant failed: $*" >&2; exit 1; }; }

grant bughunt-hunt --from file --select list,read_text --on root="$root"
