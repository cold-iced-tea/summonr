#!/usr/bin/env bash
# Prepare a bughunt lab: a clone of the target at one commit, a repro
# harness branch, an isolated Summonr home, and the rituals inscribed and
# granted against the clone.
#
#   setup.sh LAB_DIR TARGET_REPO COMMIT [SUMMONR_BIN] [CRATE_PATH]
#
# Then:  export SUMMONR_HOME=LAB_DIR/home
#
# The harness is Rust-specific (see LIMITS.md): an empty src/bughunt_repro.rs
# declared as a #[cfg(test)] module in the crate's src/lib.rs (or
# src/main.rs), so a repro test can reach crate internals. trial runs
# `cargo test bughunt_repro` on that crate.
#
# CRATE_PATH names the crate inside a Cargo workspace (e.g. crates/foo). The
# repro file stays at the repository's src/bughunt_repro.rs, the path the
# rituals write, and the crate declares it with #[path]. A target that does
# not commit Cargo.lock gets one resolved offline into the harness commit;
# fetch the dependencies first (benchmark/fetch_external.py does).
set -euo pipefail
lab="$(mkdir -p "$1" && cd "$1" && pwd)"; repo="$2"; commit="$3"
bin="${4:-summonr}"
crate_path="${5:-}"; crate_path="${crate_path%/}"
here="$(cd "$(dirname "$0")" && pwd)"
target="$lab/target"
case "$crate_path" in
  /*|*..*) echo "CRATE_PATH must be a relative path inside the repository: $crate_path" >&2; exit 2 ;;
esac

if [ ! -d "$target/.git" ]; then
  git clone -q "$repo" "$target"
fi
git -C "$target" -c advice.detachedHead=false checkout -q -B bughunt-base "$commit"
git -C "$target" config user.name bughunt
git -C "$target" config user.email bughunt@localhost

crate="${crate_path:+$crate_path/}"
[ -f "$target/${crate}Cargo.toml" ] || { echo "no Cargo.toml at ${crate_path:-the repository root}" >&2; exit 1; }
entry=${crate}src/main.rs; only=--bins
[ -f "$target/${crate}src/lib.rs" ] && { entry=${crate}src/lib.rs; only=--lib; }
[ -f "$target/$entry" ] || { echo "no src/lib.rs or src/main.rs in ${crate_path:-the repository root}" >&2; exit 1; }
manifest=()
decl='mod bughunt_repro;'
if [ -n "$crate_path" ]; then
  manifest=(--manifest-path "${crate}Cargo.toml")
  # #[path] is relative to the directory of the file declaring the module.
  up="$(printf '%s/src' "$crate_path" | sed -e 's|[^/][^/]*|..|g')"
  decl="#[path = \"$up/src/bughunt_repro.rs\"]
mod bughunt_repro;"
fi
if ! grep -q 'mod bughunt_repro;' "$target/$entry"; then
  mkdir -p "$target/src"
  : > "$target/src/bughunt_repro.rs"
  printf '\n#[cfg(test)]\n%s\n' "$decl" >> "$target/$entry"
  # A #![no_std] crate does not name std, so the repro could not reach it
  # (`extern crate std as _;` binds no name). Tests always link std.
  if grep -q '^#!\[no_std\]' "$target/$entry" && ! grep -q '^extern crate std;' "$target/$entry"; then
    printf '#[cfg(test)]\nextern crate std;\n' >> "$target/$entry"
  fi
  git -C "$target" add -f src/bughunt_repro.rs "$entry"
  if ! git -C "$target" ls-files --error-unmatch Cargo.lock >/dev/null 2>&1; then
    # Libraries rarely commit a lockfile; trial runs with --offline, so pin
    # the dependency versions already in the local Cargo cache.
    (cd "$target" && cargo generate-lockfile --offline -q "${manifest[@]}") \
      || { echo "cannot resolve dependencies offline; run benchmark/fetch_external.py (or cargo fetch) first" >&2; exit 1; }
    git -C "$target" add -f Cargo.lock
  fi
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
# model provider with multi-line prompts on stdin. The prompt becomes one
# argument, so cap it well under the kernel's 128 KiB per-argument limit and
# fail with a clear message instead of E2BIG.
cat > "$lab/bin/agy-stdin" <<'SH'
#!/bin/sh
max=65536
prompt="$(head -c $((max + 1)); printf x)"; prompt="${prompt%x}"
if [ "$(printf %s "$prompt" | wc -c)" -gt "$max" ]; then
  cat >/dev/null
  echo "agy-stdin: prompt is over $max bytes; shorten the step's prompt" >&2
  exit 2
fi
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
    # --cap-lints=warn: a target's own #![deny(warnings)] must not fail the
    # build on lints a newer compiler added (unexpected_cfgs, ...).
    test: timeout 300 env CARGO_TARGET_DIR=$lab/cargo-target RUSTFLAGS=--cap-lints=warn cargo test --offline -q ${manifest[*]:+${manifest[*]} }$only bughunt_repro
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
    # The read-only snapshot, like gemini: never the checkout trial uses.
    args: [exec, --sandbox, read-only, --skip-git-repo-check, -C, $snap, "-"]
    cwd: $snap
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
