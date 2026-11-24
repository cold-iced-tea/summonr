#!/usr/bin/env python3
"""Execution-verified SWE-bench-style repro verifier for bughunt-test.

Given a repository, a fix commit, and a bughunt-test result JSON (either the
raw ritual output {"results": [...]} or a summons envelope {"result": {"results": [...]}}),
this script replays every `verdict == "confirmed"` repro test on BOTH:

  1. FIX_COMMIT^ (the buggy parent commit)  -> must fail with BUGHUNT-CONFIRMED
  2. FIX_COMMIT  (the fixed commit)         -> must pass (exit 0)

Verdict per confirmed finding:
  - verified_fix:          fails with BUGHUNT-CONFIRMED on parent AND passes on fix
  - still_fails_on_fix:    fails with BUGHUNT-CONFIRMED on parent AND still fails on fix
                           (another real bug still present after fix, or overly strict test)
  - compile_error_on_fix:  reproduced on parent, but fix changed signatures/API so test
                           no longer compiles on fix commit
  - passed_on_parent:      did not reproduce on parent
  - compile_error_on_parent: failed to compile on parent
  - missing_test_file:     confirmed item had empty test_file

Usage:
  verify_repros.py REPO FIX_COMMIT TEST_JSON [--out VERIFY_JSON] [--timeout 120]
  verify_repros.py REPO --suite SUITE_JSON --name CASE_NAME TEST_JSON [--out VERIFY_JSON]
"""
import argparse
import json
import pathlib
import subprocess
import sys
import tempfile


def run(cmd, cwd=None, timeout=120):
    try:
        p = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        return p.returncode, p.stdout + "\n" + p.stderr
    except subprocess.TimeoutExpired as e:
        out = (e.stdout or "") + "\n" + (e.stderr or "")
        return -1, f"TIMEOUT after {timeout}s\n{out}"


def git_rev(repo, rev):
    rc, out = run(["git", "-C", str(repo), "rev-parse", "--short", rev])
    if rc != 0:
        raise RuntimeError(f"git rev-parse {rev} failed: {out.strip()}")
    return out.strip()


def find_crate_root(wt: pathlib.Path) -> pathlib.Path:
    for name in ("src/lib.rs", "src/main.rs"):
        p = wt / name
        if p.exists():
            return p
    raise RuntimeError(f"no src/lib.rs or src/main.rs found in {wt}")


def inject_and_test(wt: pathlib.Path, test_source: str, timeout: int):
    repro_path = wt / "src" / "bughunt_repro.rs"
    root_path = find_crate_root(wt)
    orig_root = root_path.read_text(encoding="utf-8")
    mod_decl = "\n#[cfg(test)]\nmod bughunt_repro;\n"
    try:
        repro_path.write_text(test_source, encoding="utf-8")
        if "mod bughunt_repro;" not in orig_root:
            root_path.write_text(orig_root + mod_decl, encoding="utf-8")
        rc, output = run(
            ["cargo", "test", "--offline", "bughunt_repro", "--", "--nocapture"],
            cwd=wt,
            timeout=timeout,
        )
        tail = "\n".join(output.strip().splitlines()[-25:])
        compiled = not (
            "could not compile" in output
            or "error[E" in output
            or "aborting due to" in output
        )
        confirmed_marker = "BUGHUNT-CONFIRMED" in output and rc != 0
        return {
            "returncode": rc,
            "compiled": compiled,
            "passed": rc == 0,
            "confirmed_marker": confirmed_marker,
            "tail": tail,
        }
    finally:
        root_path.write_text(orig_root, encoding="utf-8")
        if repro_path.exists():
            repro_path.unlink()


def extract_results(doc):
    if isinstance(doc, dict):
        if isinstance(doc.get("results"), list):
            return doc["results"]
        if isinstance(doc.get("result"), dict) and isinstance(doc["result"].get("results"), list):
            return doc["result"]["results"]
        if isinstance(doc.get("data"), dict):
            res = doc["data"].get("result")
            if isinstance(res, dict) and isinstance(res.get("results"), list):
                return res["results"]
    raise ValueError("unrecognized bughunt-test JSON structure (expected results, result.results, or data.result.results)")


def main():
    p = argparse.ArgumentParser(description="Verify bughunt-test confirmed repros against parent and fix commits.")
    p.add_argument("repo", type=pathlib.Path)
    p.add_argument("fix_commit", nargs="?")
    p.add_argument("test_json", type=pathlib.Path)
    p.add_argument("--suite", type=pathlib.Path)
    p.add_argument("--name")
    p.add_argument("--out", type=pathlib.Path)
    p.add_argument("--timeout", type=int, default=180)
    args = p.parse_args()

    fix_commit = args.fix_commit
    case_name = args.name
    category = None
    if args.suite and args.name:
        suite = json.load(open(args.suite, encoding="utf-8"))
        match = next((c for c in suite.get("cases", []) if c["name"] == args.name), None)
        if not match:
            sys.exit(f"case `{args.name}` not found in {args.suite}")
        fix_commit = fix_commit or match.get("fix_commit")
        category = match.get("category")
    if not fix_commit:
        sys.exit("fix_commit is required (pass positionally or via --suite + --name)")

    parent_commit = git_rev(args.repo, f"{fix_commit}^")
    fix_short = git_rev(args.repo, fix_commit)
    doc = json.load(open(args.test_json, encoding="utf-8"))
    results = extract_results(doc)
    confirmed = [r for r in results if r.get("verdict") == "confirmed"]

    summary = {
        "name": case_name or f"fix-{fix_short}",
        "category": category,
        "parent_commit": parent_commit,
        "fix_commit": fix_short,
        "total_results": len(results),
        "confirmed_input": len(confirmed),
        "counts": {
            "verified_fix": 0,
            "still_fails_on_fix": 0,
            "compile_error_on_fix": 0,
            "passed_on_parent": 0,
            "compile_error_on_parent": 0,
            "missing_test_file": 0,
        },
        "items": [],
    }

    if not confirmed:
        if args.out:
            args.out.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
        print(f"{summary['name']}: 0 confirmed findings to verify")
        return

    with tempfile.TemporaryDirectory(prefix="bughunt-verify-") as tmp:
        wt_parent = pathlib.Path(tmp) / "parent"
        wt_fix = pathlib.Path(tmp) / "fix"
        try:
            for wt, rev in ((wt_parent, parent_commit), (wt_fix, fix_short)):
                rc, out = run(["git", "-C", str(args.repo), "worktree", "add", "--detach", str(wt), rev])
                if rc != 0:
                    raise RuntimeError(f"git worktree add {rev} failed: {out}")

            for idx, item in enumerate(confirmed, 1):
                src = (item.get("test_file") or "").strip()
                entry = {
                    "index": idx,
                    "file": item.get("file"),
                    "diagnosis": item.get("diagnosis"),
                }
                if not src:
                    entry["status"] = "missing_test_file"
                    summary["counts"]["missing_test_file"] += 1
                    summary["items"].append(entry)
                    continue

                on_parent = inject_and_test(wt_parent, src, args.timeout)
                entry["parent"] = on_parent
                if not on_parent["compiled"]:
                    status = "compile_error_on_parent"
                elif not on_parent["confirmed_marker"]:
                    status = "passed_on_parent"
                else:
                    on_fix = inject_and_test(wt_fix, src, args.timeout)
                    entry["fix"] = on_fix
                    if not on_fix["compiled"]:
                        status = "compile_error_on_fix"
                    elif on_fix["passed"]:
                        status = "verified_fix"
                    else:
                        status = "still_fails_on_fix"

                entry["status"] = status
                summary["counts"][status] += 1
                summary["items"].append(entry)
        finally:
            for wt in (wt_parent, wt_fix):
                if wt.exists():
                    run(["git", "-C", str(args.repo), "worktree", "remove", "--force", str(wt)])

    if args.out:
        args.out.write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")

    c = summary["counts"]
    print(
        f"{summary['name']} ({parent_commit}->{fix_short}): "
        f"confirmed={summary['confirmed_input']} | "
        f"verified_fix={c['verified_fix']} still_fails={c['still_fails_on_fix']} "
        f"compile_err_fix={c['compile_error_on_fix']} parent_fail={c['passed_on_parent'] + c['compile_error_on_parent']}"
    )


if __name__ == "__main__":
    main()
