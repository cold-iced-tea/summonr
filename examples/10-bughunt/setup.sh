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

entry=src/main.rs; only=--bins
[ -f "$target/src/lib.rs" ] && { entry=src/lib.rs; only=--lib; }
if ! grep -q '^mod bughunt_repro;' "$target/$entry"; then
  : > "$target/src/bughunt_repro.rs"
  printf '\n#[cfg(test)]\nmod bughunt_repro;\n' >> "$target/$entry"
  git -C "$target" add src/bughunt_repro.rs "$entry"
  git -C "$target" commit -q -m "bughunt: repro harness"
fi

# git.worktree_add insists the trunk matches <remote>/<trunk>, so give the
# clone a local bare remote holding the base branch (see LIMITS.md).
if [ ! -d "$lab/remote.git" ]; then git init -q --bare "$lab/remote.git"; fi
git -C "$target" remote remove lab 2>/dev/null || true
git -C "$target" remote add lab "$lab/remote.git"
git -C "$target" push -q --force lab bughunt-base

# A read-only snapshot of the source for agent CLIs (Gemini via agy): they
# have their own file tools, so they get a copy they cannot write to, never
# the checkout (AGENT-NOTES 34).
snap="$lab/readonly-src"
if [ -d "$snap" ]; then chmod -R u+w "$snap"; rm -rf "$snap"; fi
mkdir -p "$snap"
git -C "$target" archive HEAD | tar -x -C "$snap"
chmod -R a-w "$snap"

export SUMMONR_HOME="$lab/home"
mkdir -p "$SUMMONR_HOME/config" "$SUMMONR_HOME/rituals" "$lab/bin"
# Tiny stdin-to-flag wrapper so agy -p works as a first-class kind: command
# model provider with multi-line prompts on stdin.
cat > "$lab/bin/agy-stdin" <<'SH'
#!/bin/sh
prompt="$(cat)"
exec agy --model gemini-4-neon-medium --sandbox --disable-slash-commands --print-timeout 600s -p "$prompt"
SH
chmod +x "$lab/bin/agy-stdin"
cat > "$SUMMONR_HOME/config/config.yaml" <<EOF
ollama_url: "${OLLAMA_HOST:-http://127.0.0.1:11434}"
repos:
  bughunt-target:
    path: $target
    trunk: bughunt-base
    remote: lab
    # timeout: a repro that hangs must not use up the whole run (no shell
    # here, so timeout and env are plain programs). -q keeps the tail short.
    test: timeout 300 env CARGO_TARGET_DIR=$lab/cargo-target cargo test --offline -q $only bughunt_repro
    disposable: [target/, Cargo.lock]
# Optional command-backed cloud model providers (test.yaml model: {one_of, choose}).
providers:
  gemini:
    kind: command
    program: $lab/bin/agy-stdin
    cwd: $snap
    env: [HOME, PATH]
    stdin: prompt
    timeout_ms: 600000
  codex:
    kind: command
    program: codex
    args: [exec, --sandbox, read-only, --skip-git-repo-check, -C, $target, "-"]
    cwd: $target
    env: [HOME, PATH]
    stdin: prompt
    timeout_ms: 600000
EOF

# Rewrite root placeholders into a staging dir and inscribe the directory in
# automatic dependency order.
rm -f "$SUMMONR_HOME/rituals"/*.yaml
for f in "$here"/*.yaml; do
  base="$(basename "$f")"
  case "$base" in *.drill.yaml|*.rite.yaml|*.assay.yaml) continue ;; esac
  sed -e "s|/absolute/path/to/lab|$lab|g" \
      -e "s|/absolute/path/to/target-repro|$target-repro|g" \
      -e "s|/absolute/path/to/target|$target|g" "$f" > "$SUMMONR_HOME/rituals/$base"
done
"$bin" inscribe "$SUMMONR_HOME/rituals" --replace --json >/dev/null \
  || { echo "inscribe failed" >&2; "$bin" inscribe "$SUMMONR_HOME/rituals" --replace --json >&2; exit 1; }
"$here/grant.sh" "$bin" "$target" "$lab"
mkdir -p "$lab/benchmark"
cp "$here"/benchmark/*.json "$lab/benchmark/" 2>/dev/null || true
# The core reads config.yaml at start.
"$bin" core restart --json >/dev/null
echo "lab ready: export SUMMONR_HOME=$SUMMONR_HOME"
