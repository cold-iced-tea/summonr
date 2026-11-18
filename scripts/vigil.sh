#!/usr/bin/env bash
# Open the instrument in a tmux popup, or use the current terminal.
set -euo pipefail
summonr_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
summonr_binary=${SUMMONR_BIN:-"$summonr_root/target/debug/summonr"}
if [[ ! -x "$summonr_binary" ]]; then
    printf '%s\n' "Build Summonr first with cargo build, or set SUMMONR_BIN to its executable." >&2
    exit 1
fi
if [[ -n "${TMUX:-}" ]]; then
    exec tmux display-popup -E -w 78 -h 22 -d "$PWD" "$summonr_binary" daemon
fi
exec "$summonr_binary" daemon
