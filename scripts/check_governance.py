#!/usr/bin/env python3
"""End-to-end check for v0.2 Enterprise Governance & Compliance:
- Operator provisioning (`policy_author`, `agent_runner`, `auditor`)
- Normalized RBAC CLI (`rbac list`, `role-create`, `grant`, `bind`, `unbind`, `revoke`)
- Runnable redaction example (`examples/09-redact-document`) with petition hold & approval
- Unified `audit verify` (exit 0 on clean chain, exit 15 on SQLite row tamper)
- `audit export --format json` & `--format ndjson` (stdout SIEM stream & file bundle)
- Offline `audit verify-bundle` (pass on clean bundle, fail exit 15 on tampered bundle)
Runs completely offline with zero inference.
"""
import json
import os
import pathlib
import sqlite3
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / "target/debug/summonr"


def cli(home, *args, ok=True, parse_json=True):
    env = dict(os.environ, SUMMONR_HOME=str(home))
    cmd = [str(BIN), *map(str, args)]
    if parse_json and "--json" not in cmd:
        cmd.append("--json")
    p = subprocess.run(cmd, env=env, text=True, capture_output=True, timeout=20)
    if ok and p.returncode != 0:
        raise AssertionError((args, p.returncode, p.stdout, p.stderr))
    if not ok and p.returncode == 0:
        raise AssertionError(("expected failure", args, p.stdout))
    if parse_json:
        return json.loads(p.stdout), p.returncode
    return p.stdout, p.returncode


def main():
    with tempfile.TemporaryDirectory(prefix="summonr-governance-") as td:
        tmp = pathlib.Path(td)
        home = tmp / "home"
        intake = tmp / "intake"
        out_dir = tmp / "redacted"
        intake.mkdir()
        out_dir.mkdir()
        (intake / "case-4817.txt").write_text(
            "Patient: Jane Doe\nSSN: 123-45-6789\nAccount: ACCT-998811\nNote: Routine follow-up.\n"
        )

        try:
            # 1. Core identity & operator provisioning.
            ident, _ = cli(home, "identity")
            core_key = ident["data"]["key"]

            cli(home, "operator", "new", "alice", "policy_author")
            cli(home, "operator", "new", "runner", "agent_runner")
            cli(home, "operator", "new", "carol", "auditor")
            ops, _ = cli(home, "operator", "list")
            assert len(ops["data"]) == 3, ops

            # 2. Custom RBAC role, permission grant/revoke, actor bind/unbind.
            cli(home, "rbac", "role-create", "file-reader", "Read-only file access")
            cli(home, "rbac", "grant", "file-reader", "reagent", "file.read_text", "read")
            cli(home, "rbac", "bind", "operator:carol", "file-reader")
            rb, _ = cli(home, "rbac", "list")
            assert any(r["name"] == "file-reader" for r in rb["data"]["roles"]), rb
            assert any(
                b["actor_id"] == "operator:carol" and b["role"] == "file-reader" and b["revoked_at"] is None
                for b in rb["data"]["bindings"]
            ), rb
            cli(home, "rbac", "unbind", "operator:carol", "file-reader")
            cli(home, "rbac", "revoke", "file-reader", "reagent", "file.read_text", "read")

            # 3. Runnable redaction example (09-redact-document) with held write + approval.
            src_yaml = (ROOT / "examples/09-redact-document/redact-document.yaml").read_text()
            ritual_path = tmp / "redact-document.yaml"
            ritual_path.write_text(
                src_yaml.replace("/absolute/path/to/intake", str(intake))
                .replace("/absolute/path/to/redacted", str(out_dir))
            )
            cli(home, "inscribe", ritual_path)
            cli(home, "allow", "redact-document", "--from", "file", "--select", "read_text", "--on", f"root={intake}")
            cli(
                home,
                "allow",
                "redact-document",
                "--from",
                "file",
                "--select",
                "write_text,stat",
                "--on",
                f"root={out_dir}",
            )

            run_res, _ = cli(
                home,
                "run",
                "redact-document",
                "--arg",
                "path=case-4817.txt",
                "--wait",
            )
            assert run_res["data"]["outcome"] == "returned", run_res
            # Write was held as a petition — destination file must NOT exist yet.
            assert not (out_dir / "case-4817.txt").exists()

            pets, _ = cli(home, "petition")
            pending = [p for p in pets["data"] if p["status"] == "pending"]
            assert len(pending) == 1, pets
            cli(home, "petition", "permit", pending[0]["id"])
            redacted_text = (out_dir / "case-4817.txt").read_text()
            assert "123-45-6789" not in redacted_text, redacted_text
            assert "[REDACTED-SSN]" in redacted_text, redacted_text

            # 4. Verify tamper-evident audit chain.
            ver, rc = cli(home, "audit", "verify")
            assert rc == 0 and ver["data"]["ok"] is True, ver

            # 5. Export JSON (stdout), NDJSON (SIEM stdout), and signed file bundle.
            json_out, _ = cli(home, "audit", "export", "--format", "json")
            entries = json_out["data"]["bundle"]["audit_log"]
            assert any(e["action"] == "summon.finished" for e in entries), entries

            ndjson_out, _ = cli(home, "audit", "export", "--format", "ndjson", parse_json=False)
            lines = [ln for ln in ndjson_out.splitlines() if ln.strip()]
            assert len(lines) >= 4, ndjson_out
            manifest = json.loads(lines[-1])
            assert manifest["record_type"] == "summonr-siem-manifest-v1", manifest

            bundle_path = tmp / "evidence.bundle.json"
            cli(home, "audit", "export", bundle_path, "--format", "json")
            vb, rc = cli(home, "audit", "verify-bundle", bundle_path, "--key", core_key)
            assert rc == 0 and vb["data"]["ok"] is True, vb

            # Tampered bundle must fail offline verification with exit 15.
            bad_bundle = json.loads(bundle_path.read_text())
            bad_bundle["bundle"]["controls"]["active_operators"] = 999
            bad_path = tmp / "bad.bundle.json"
            bad_path.write_text(json.dumps(bad_bundle))
            _, bad_rc = cli(home, "audit", "verify-bundle", bad_path, "--key", core_key, ok=False)
            assert bad_rc == 15, bad_rc

            # 6. Simulate offline SQLite tampering -> `audit verify` must detect & exit 15.
            cli(home, "core", "stop")
            db_path = home / "state/summonr.db"
            conn = sqlite3.connect(str(db_path))
            conn.executescript(
                "DROP TRIGGER IF EXISTS audit_log_no_update;"
                "UPDATE audit_log SET outcome='forged' WHERE seq=1;"
            )
            conn.close()

            tamper_out, tamper_rc = cli(home, "audit", "verify", ok=False)
            assert tamper_rc == 15, (tamper_rc, tamper_out)
            assert tamper_out["data"]["ok"] is False, tamper_out
            assert tamper_out["data"]["problem"]["seq"] == 1, tamper_out

            print("PASS: governance RBAC CLI, redact-document hold/permit, SIEM export, bundle verify, SQLite tamper detection")
        finally:
            subprocess.run(
                [str(BIN), "core", "stop"],
                env=dict(os.environ, SUMMONR_HOME=str(home)),
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )


if __name__ == "__main__":
    main()
