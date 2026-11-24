"""Prepare a fresh Summonr home pinned to a local test project."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

from lab import HERE, dotted, git, patch_for

ROOT = HERE.parent


def demo(repo):
    repo.mkdir()
    git("init", "-q", cwd=repo)
    (repo / "clockface.py").write_text(
        'def display(minutes):\n    return f"{minutes // 60}:{minutes % 60}"\n')
    (repo / "test_clockface.py").write_text(
        'import unittest\n\nstyle = "short"\n\nclass ClockTests(unittest.TestCase):\n'
        '    def test_switch(self):\n        global style\n        style = "long"\n\n'
        '    def test_default(self):\n        self.assertEqual(style, "short")\n')
    for message in ["Original display", "Pad displayed minutes"]:
        if message.startswith("Pad"):
            (repo / "clockface.py").write_text(
                'def display(minutes):\n    return f"{minutes // 60}:{minutes % 60:02d}"\n')
        git("add", ".", cwd=repo)
        git("-c", "user.name=Test Lab", "-c", "user.email=lab@localhost",
            "-c", "commit.gpgsign=false", "commit", "-qm", message, cwd=repo)
    return {"repo": str(repo), "head": "HEAD", "parent": "HEAD^",
            "target": "test_clockface.ClockTests.test_default",
            "predecessor": "test_clockface.ClockTests.test_switch",
            "report": {"title": "Minute display drops the leading zero", "module": "clockface",
                       "function": "display", "args": [65], "expected": "1:05"}}


def configure(home, raw):
    spec = dict(raw)
    repo = Path(spec["repo"]).resolve(strict=True)
    spec["repo"] = str(repo)
    for ref in ("parent", "head"):
        spec[ref] = git("rev-parse", "--verify", "--end-of-options",
                        spec[ref] + "^{commit}", cwd=repo)
    spec.setdefault("timeout", 10)
    if type(spec["timeout"]) is not int or not 1 <= spec["timeout"] <= 30:
        raise ValueError("timeout must be between 1 and 30 seconds per execution")
    stages = {}
    if "target" in spec or "predecessor" in spec:
        if not all(dotted(spec[k]) for k in ("target", "predecessor")) or spec["target"] == spec["predecessor"]:
            raise ValueError("Choose two different fully qualified unittest method names")
        stages.update({"court-probe": ("write", "probe", None),
                       "court-hearing": ("write", "challenge", "plan")})
    if "report" in spec:
        _, source, _, _ = patch_for(spec["report"])
        if len(source.encode()) > 8000 or not isinstance(spec["report"]["title"], str):
            raise ValueError("Expected a short report with a text title")
        stages.update({"report-draft": ("read", "draft", None),
                       "report-verify": ("write", "verify", "digest")})
    if not stages:
        raise ValueError("Provide court test names or a structured report")
    raw_spec = json.dumps(spec, indent=2, allow_nan=False).encode()
    spec_path = home / ("case-" + hashlib.sha256(raw_spec).hexdigest() + ".json")
    spec_path.write_bytes(raw_spec)
    commands = {}
    for name, (effect, action, parameter) in stages.items():
        argv = ["-B", str(HERE), str(spec_path), action]
        cmd = {"program": sys.executable, "cwd": str(home), "args": argv, "effect": effect, "env": ["PATH"]}
        if parameter:
            argv.append("${" + parameter + "}")
            cmd["params"] = {parameter: {"max_length": 64} if parameter == "digest" else
                              {"enum": ["order", "revision", "repeat", "stop"]}}
        commands[name] = cmd
    (home / "config").mkdir()
    (home / "config/config.yaml").write_text(json.dumps({"commands": commands}, indent=2))
    return stages


def main():
    p = argparse.ArgumentParser()
    p.add_argument("home", type=Path, help="new directory for the lab")
    p.add_argument("--spec", type=Path, help="trusted local project case JSON (default: clock demo)")
    p.add_argument("--configure-only", action="store_true", help="prepare config and fixtures without contacting a core")
    p.add_argument("--bin", default="summonr")
    args = p.parse_args()
    home = args.home.resolve()
    home.mkdir(mode=0o700, parents=True)
    raw = json.loads(args.spec.read_text()) if args.spec else demo(home / "fixture")
    stages = configure(home, raw)
    if args.configure_only:
        return
    env = dict(os.environ, SUMMONR_HOME=str(home))
    for name, (effect, _, _) in stages.items():
        subprocess.run([args.bin, "inscribe", str(ROOT / "rituals" / (name + ".yaml"))], env=env, check=True)
        subprocess.run([args.bin, "allow", name, "--from", "cmd", "--select", effect], env=env, check=True)
    print("SUMMONR_HOME=" + str(home))


if __name__ == "__main__":
    main()
