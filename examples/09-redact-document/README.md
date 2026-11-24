# Example 09 — Active Boundary Enforcement & Redaction

A self-contained, zero-inference redaction ritual demonstrating Summonr's enterprise governance controls end-to-end:

- **Scoped filesystem roots:** The intake and output directories are inputs, granted with `allow --under`: read-only under the intake directory, write-and-stat under the output directory. The run holds only the roots it was given (no `..`, no symlinks, no writes back to intake).
- **Owner-locked review gate:** `hold: true` is marked `user_only: true`, so remote agents/familiars cannot override it.
- **Digest-bound petition approval:** `write_out` pauses as a petition until an authorized reviewer approves it, then `verify: file.stat` confirms the written file.
- **Tamper-evident audit trail:** Every grant, invocation, petition decision, and finished run is hash-chained and Ed25519-signed in `audit_log`.

## Run in 60 seconds

From the repository root (after `cargo build`):

```bash
export PATH="$PWD/target/debug:$PATH"
export SUMMONR_HOME="$(mktemp -d)"
OUT_DIR="$(mktemp -d)"

# The ritual takes its intake and output directories as inputs; grant the
# directories they may lie under, each for only the operations it needs
summonr inscribe examples/09-redact-document/redact-document.yaml
summonr allow redact-document --from file --select read_text --under "$PWD/examples/09-redact-document/intake"
summonr allow redact-document --from file --select write_text,stat --under "$OUT_DIR"

# Run: write_out pauses as a petition (nothing written yet)
summonr run redact-document --arg intake="$PWD/examples/09-redact-document/intake" \
  --arg out="$OUT_DIR" --arg path=case-4817.txt --wait

# Inspect and approve the held write
summonr petition
summonr petition permit <PETITION_ID>
cat "$OUT_DIR/case-4817.txt"

# Verify the tamper-evident audit chain & export SIEM/SOC 2 evidence
summonr audit verify
summonr audit export --format json
summonr core stop
```
