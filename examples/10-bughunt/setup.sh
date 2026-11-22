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

export SUMMONR_HOME="$lab/home"
mkdir -p "$SUMMONR_HOME/config"
cat > "$SUMMONR_HOME/config/config.yaml" <<EOF
ollama_url: "${OLLAMA_HOST:-http://127.0.0.1:11434}"
repos:
  bughunt-target:
    path: $target
    trunk: bughunt-base
    remote: lab
    test: env CARGO_TARGET_DIR=$lab/cargo-target cargo test --offline $only bughunt_repro
    disposable: [target/]
# Optional cloud escalation (test.yaml, escalate=codex, critical findings
# only). Needs the codex CLI on PATH and logged in. Read-only sandbox: codex
# prints the test file; Summonr writes it.
commands:
  bughunt-codex:
    program: codex
    args: [exec, --sandbox, read-only, --skip-git-repo-check, -C, $target, "\${prompt}"]
    cwd: $target
    effect: read
    params:
      prompt: {max_length: 20000}
EOF

# Children before parents: a ritual step needs its child already inscribed.
for name in verdict batch-ready gather hunt test score; do
  f="$here/$name.yaml"; [ -f "$f" ] || continue
  sed -e "s|/absolute/path/to/target-repro|$target-repro|g" \
      -e "s|/absolute/path/to/target|$target|g" "$f" > "$SUMMONR_HOME/$(basename "$f")"
  "$bin" inscribe "$SUMMONR_HOME/$(basename "$f")" --replace --json >/dev/null \
    || { echo "inscribe failed: $name" >&2; "$bin" inscribe "$SUMMONR_HOME/$(basename "$f")" --json >&2; exit 1; }
done
"$here/grant.sh" "$bin" "$target"
# The core reads config.yaml at start.
"$bin" core restart --json >/dev/null
echo "lab ready: export SUMMONR_HOME=$SUMMONR_HOME"
