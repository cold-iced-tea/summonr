#!/usr/bin/env bash
# Prepare a bughunt lab: a clone of the target at one commit, a repro
# harness branch, an isolated Summonr home, and the rituals inscribed and
# granted against the clone.
#
#   setup.sh LAB_DIR TARGET_REPO COMMIT [SUMMONR_BIN]
#
# Then:  export SUMMONR_HOME=LAB_DIR/home
#
# The harness is Rust-specific (see LIMITS.md): an empty src/bughunt_repro.rs
# declared as a #[cfg(test)] module in src/main.rs (or src/lib.rs), so a repro
# test can reach crate internals. trial runs `cargo test bughunt_repro`.
set -euo pipefail
lab="$(mkdir -p "$1" && cd "$1" && pwd)"; repo="$2"; commit="$3"
bin="${4:-summonr}"
here="$(cd "$(dirname "$0")" && pwd)"
target="$lab/target"

if [ ! -d "$target/.git" ]; then
  git clone -q "$repo" "$target"
fi
git -C "$target" -c advice.detachedHead=false checkout -q -B bughunt-base "$commit"
git -C "$target" config user.name bughunt
git -C "$target" config user.email bughunt@localhost

entry=src/main.rs; [ -f "$target/src/lib.rs" ] && entry=src/lib.rs
if ! grep -q '^mod bughunt_repro;' "$target/$entry"; then
  : > "$target/src/bughunt_repro.rs"
  printf '\n#[cfg(test)]\nmod bughunt_repro;\n' >> "$target/$entry"
  git -C "$target" add src/bughunt_repro.rs "$entry"
  git -C "$target" commit -q -m "bughunt: repro harness"
fi

export SUMMONR_HOME="$lab/home"
mkdir -p "$SUMMONR_HOME"
cat > "$SUMMONR_HOME/config.yaml" <<EOF
ollama_url: "${OLLAMA_HOST:-http://127.0.0.1:11434}"
repos:
  bughunt-target:
    path: $target
    trunk: bughunt-base
    remote: origin
    test: env CARGO_TARGET_DIR=$lab/cargo-target cargo test --offline bughunt_repro
    disposable: [target/]
EOF

for f in "$here"/*.yaml; do
  case "$f" in *.drill.yaml|*.rite.yaml) continue ;; esac
  sed -e "s|/absolute/path/to/target-repro|$target-repro|g" \
      -e "s|/absolute/path/to/target|$target|g" "$f" > "$SUMMONR_HOME/$(basename "$f")"
  "$bin" inscribe "$SUMMONR_HOME/$(basename "$f")" --json >/dev/null
done
"$here/grant.sh" "$bin" "$target"
echo "lab ready: export SUMMONR_HOME=$SUMMONR_HOME"
