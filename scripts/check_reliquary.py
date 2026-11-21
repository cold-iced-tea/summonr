#!/usr/bin/env python3
"""End-to-end check for Reliquary (registry): two cores over loopback listen,
signed pack publish/browse/show/inscribe, offline export/import, zero-grant
transfer, and tampered-pack rejection. Runs with zero inference.
"""
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[1]
BIN = ROOT / "target/debug/summonr"


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def cli(home, *args, ok=True):
    env = dict(os.environ, SUMMONR_HOME=str(home))
    p = subprocess.run([str(BIN), *map(str, args), "--json"], env=env, text=True, capture_output=True, timeout=20)
    if ok and p.returncode != 0:
        raise AssertionError((args, p.returncode, p.stdout, p.stderr))
    if not ok and p.returncode == 0:
        raise AssertionError(("expected failure", args, p.stdout))
    return json.loads(p.stdout)


def main():
    with tempfile.TemporaryDirectory(prefix="summonr-reliquary-") as td:
        tmp = pathlib.Path(td)
        home_a = tmp / "a"
        home_b = tmp / "b"
        port_a = free_port()
        listener = None
        try:
            id_a = cli(home_a, "identity")["data"]
            id_b = cli(home_b, "identity")["data"]

            # Pin each other as peers.
            cli(home_a, "peer", "add", "consumer", "--url", "http://127.0.0.1:1", "--key", id_b["key"])
            cli(home_b, "peer", "add", "author", "--url", f"http://127.0.0.1:{port_a}", "--key", id_a["key"])

            # Start Core A listener.
            listener = subprocess.Popen(
                [str(BIN), "listen", f"127.0.0.1:{port_a}"],
                env=dict(os.environ, SUMMONR_HOME=str(home_a)),
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            deadline = time.time() + 10
            while time.time() < deadline:
                try:
                    with socket.create_connection(("127.0.0.1", port_a), timeout=0.2):
                        break
                except OSError:
                    time.sleep(0.1)

            # Inscribe child + parent ritual on Core A, grant mail.locate on A, publish pack.
            leaf = tmp / "leaf.yaml"
            leaf.write_text(
                "ritual: leaf\ninputs:\n  x: {type: string}\nessence: {requests: 0, time: 1m}\n"
                "steps:\n  - id: s1\n    operation: data.trim\n    arguments: {text: \"${inputs.x}\"}\n"
                "return:\n  out: \"${steps.s1.output}\"\n"
            )
            parent = tmp / "parent.yaml"
            parent.write_text(
                "ritual: parent\ninputs:\n  x: {type: string}\nrequires:\n  - reagent: mail\n    action: locate\n"
                "essence: {requests: 0, time: 1m}\nsteps:\n  - id: c1\n    ritual: leaf\n    arguments: {x: \"${inputs.x}\"}\n"
                "  - id: loc\n    operation: mail.locate\n    arguments: {message: \"${steps.c1.output.out}\"}\n"
                "return:\n  out: \"${steps.c1.output.out}\"\n"
            )
            cli(home_a, "inscribe", leaf)
            cli(home_a, "inscribe", parent)
            cli(home_a, "grant", "parent", "--from", "mail", "--select", "locate")
            pub = cli(home_a, "reliquary", "publish", "parent")["data"]
            assert pub["children"] == 1, pub

            # Core B subscribes to Core A, browses catalog, shows pack, and inscribes over peer RPC.
            cli(home_b, "reliquary", "add", "author")
            cat = cli(home_b, "reliquary", "list", "author")["data"]
            assert len(cat["packs"]) == 1 and cat["packs"][0]["name"] == "parent", cat

            shown = cli(home_b, "reliquary", "show", "author", "parent")["data"]
            assert shown["verified"] is True and shown["grants_included"] is False, shown

            ins = cli(home_b, "reliquary", "inscribe", "author", "parent")["data"]
            assert ins["grants_transferred"] is False, ins
            assert len(ins["inscribed"]) == 2, ins
            assert len(ins["missing_grants"]) == 1, ins

            # Offline export & tampered-pack rejection.
            pack_file = tmp / "parent.pack.json"
            cli(home_a, "reliquary", "export", "parent", pack_file)
            tampered = json.loads(pack_file.read_text())
            tampered["manifest"]["description"] = "forged"
            bad_file = tmp / "bad.pack.json"
            bad_file.write_text(json.dumps(tampered))
            bad = cli(home_b, "reliquary", "import", bad_file, "--key", id_a["key"], ok=False)
            assert bad["ok"] is False, bad

            print("PASS: reliquary peer publish/browse/show/inscribe, zero-grant transfer, offline export, tampered pack rejected")
        finally:
            if listener:
                listener.terminate()
                listener.wait(timeout=5)
            for h in (home_a, home_b):
                subprocess.run([str(BIN), "core", "stop"], env=dict(os.environ, SUMMONR_HOME=str(h)), capture_output=True)


if __name__ == "__main__":
    main()
