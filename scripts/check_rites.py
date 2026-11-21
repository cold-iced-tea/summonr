#!/usr/bin/env python3
"""End-to-end check for Rite (workflow): multi-stage execution, touchstone &
owner (petition) gates, completion vs acceptance separation, and assumption
falsification reopening downstream stages without auto-executing. Zero inference.
"""
import json
import os
import pathlib
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / "target/debug/summonr"


def cli(home, *args, ok=True):
    env = dict(os.environ, SUMMONR_HOME=str(home))
    p = subprocess.run([str(BIN), *map(str, args), "--json"], env=env, text=True, capture_output=True, timeout=20)
    if ok and p.returncode != 0:
        raise AssertionError((args, p.returncode, p.stdout, p.stderr))
    if not ok and p.returncode == 0:
        raise AssertionError(("expected failure", args, p.stdout))
    return json.loads(p.stdout)


def main():
    with tempfile.TemporaryDirectory(prefix="summonr-rite-") as td:
        tmp = pathlib.Path(td)
        home = tmp / "home"
        try:
            stage_r = tmp / "echo-stage.yaml"
            stage_r.write_text(
                "ritual: echo-stage\ninputs:\n  text: {type: string}\nessence: {requests: 0, time: 1m}\n"
                "steps:\n  - id: s1\n    operation: data.trim\n    arguments: {text: \"${inputs.text}\"}\n"
                "return:\n  out: \"${steps.s1.output}\"\n"
            )
            ts_r = tmp / "check-ok.yaml"
            ts_r.write_text(
                "ritual: check-ok\ninputs:\n  output: {type: object}\nessence: {requests: 0, time: 1m}\n"
                "steps:\n  - id: s1\n    operation: data.trim\n    arguments: {text: \"ok\"}\n"
                "return:\n  pass: true\n"
            )
            cli(home, "inscribe", stage_r)
            cli(home, "inscribe", ts_r)

            rite_file = tmp / "release.yaml"
            rite_file.write_text(
                "rite: release-prep\n"
                "inputs:\n  msg: {type: string}\n"
                "assumptions:\n  - id: base_ok\n    statement: \"Base assumptions hold\"\n"
                "stages:\n"
                "  - id: s1\n    ritual: echo-stage\n    inputs: {text: \"${inputs.msg}\"}\n    assumes: [base_ok]\n"
                "    gate:\n      type: touchstone\n      ritual: check-ok\n"
                "  - id: s2\n    ritual: echo-stage\n    after: [s1]\n    inputs: {text: \"${stages.s1.output.out}\"}\n"
                "    gate:\n      type: owner\n"
                "  - id: s3\n    ritual: echo-stage\n    after: [s2]\n    inputs: {text: \"${stages.s2.output.out}\"}\n"
            )

            reh = cli(home, "rite", "rehearse", rite_file, "--arg", "msg=  hello  ")["data"]
            assert len(reh["stages"]) == 3, reh

            started = cli(home, "workflow", "start", rite_file, "--arg", "msg=  hello  ")["data"]
            rid = started["id"]
            assert started["stages"][0]["accepted"] is True
            assert started["stages"][1]["status"] == "completed"
            assert started["stages"][1]["accepted"] is False
            assert started["stages"][1]["gate_status"] == "pending_owner"
            assert started["stages"][2]["status"] == "pending"

            pid = started["stages"][1]["petition"]
            cli(home, "petition", "permit", pid)
            done = cli(home, "rite", "advance", rid)["data"]
            assert done["status"] == "accepted", done
            assert done["stages"][2]["output"]["out"] == "hello"

            fals = cli(home, "rite", "falsify", rid, "base_ok", "--reason", "scope changed")["data"]
            assert set(fals["reopened"]) == {"s1", "s2", "s3"}, fals
            shown = cli(home, "rite", "show", rid)["data"]
            assert shown["status"] == "reopened"
            assert all(s["accepted"] is False for s in shown["stages"])

            cli(home, "rite", "assert", rid, "base_ok", "--reason", "fixed")
            resumed = cli(home, "rite", "advance", rid)["data"]
            assert resumed["stages"][0]["accepted"] is True
            assert resumed["stages"][1]["gate_status"] == "pending_owner"

            print("PASS: rite rehearse/start/advance, touchstone & owner petition gates, assumption falsify/reopen/assert")
        finally:
            subprocess.run([str(BIN), "core", "stop"], env=dict(os.environ, SUMMONR_HOME=str(home)), capture_output=True)


if __name__ == "__main__":
    main()
