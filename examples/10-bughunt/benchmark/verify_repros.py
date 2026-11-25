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
  verify_repros.py REPO FIX_COMMIT TEST_JSON [--crate-path crates/foo] [--out VERIFY_JSON] [--timeout 120]
  verify_repros.py REPO --suite SUITE_JSON --name CASE_NAME TEST_JSON [--out VERIFY_JSON]

With --suite, a case's `crate_path` is used, and when the case names a
`repo_name` (external-suite.json) REPO may be the fetch_external.py cache
directory that holds that clone. The repro goes where setup.sh puts it: the
repository's src/bughunt_repro.rs, declared from the crate's src/lib.rs (or
src/main.rs), through #[path] for a workspace crate. Each commit is tested
with `cargo test --offline` on that crate only; dependencies must already be
in the Cargo cache.
"""
import argparse
import json
import os
import pathlib
import subprocess
import sys
import tempfile


def run(cmd, cwd=None, timeout=120, env=None):
    try:
        p = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=timeout,
            env=env,
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


def find_crate_root(wt: pathlib.Path, crate_path: str = "") -> pathlib.Path:
    crate = wt / crate_path
    for name in ("src/lib.rs", "src/main.rs"):
        p = crate / name
        if p.exists():
            return p
    raise RuntimeError(f"no src/lib.rs or src/main.rs found in {crate}")


def module_decl(crate_path: str) -> str:
    """How the crate root declares the repository's src/bughunt_repro.rs."""
    if not crate_path:
        return "\n#[cfg(test)]\nmod bughunt_repro;\n"
    # #[path] is relative to the directory of the declaring file (CRATE/src).
    up = "/".join([".."] * (len(pathlib.PurePosixPath(crate_path).parts) + 1))
    return f'\n#[cfg(test)]\n#[path = "{up}/src/bughunt_repro.rs"]\nmod bughunt_repro;\n'


def inject_and_test(wt: pathlib.Path, test_source: str, timeout: int, crate_path: str = "", target_dir=None):
    src_dir = wt / "src"
    made_src = not src_dir.exists()
    repro_path = src_dir / "bughunt_repro.rs"
    root_path = find_crate_root(wt, crate_path)
    orig_root = root_path.read_text(encoding="utf-8")
    cmd = ["cargo", "test", "--offline"]
    if crate_path:
        cmd += ["--manifest-path", f"{crate_path}/Cargo.toml"]
    if root_path.name == "lib.rs":
        cmd.append("--lib")
    cmd += ["bughunt_repro", "--", "--nocapture"]
    # As in setup.sh: a target's own #![deny(warnings)] must not fail the
    # build on lints a newer compiler added.
    env = dict(os.environ, RUSTFLAGS="--cap-lints=warn")
    if target_dir:
        env["CARGO_TARGET_DIR"] = str(target_dir)
    try:
        src_dir.mkdir(exist_ok=True)
        repro_path.write_text(test_source, encoding="utf-8")
        if "mod bughunt_repro;" not in orig_root:
            decl = module_decl(crate_path)
            # As in setup.sh: a #![no_std] crate does not name std.
            lines = orig_root.splitlines()
            if any(l.startswith("#![no_std]") for l in lines) and "extern crate std;" not in lines:
                decl += "#[cfg(test)]\nextern crate std;\n"
            root_path.write_text(orig_root + decl, encoding="utf-8")
        rc, output = run(cmd, cwd=wt, timeout=timeout, env=env)
        tail = "\n".join(output.strip().splitlines()[-25:])
        compiled = not (
            "could not compile" in output
            or "error[E" in output
            or "aborting due to" in output
        )
        confirmed_marker = (
            "BUGHUNT-CONFIRMED" in output
            or "fatal runtime error: stack overflow" in output
        ) and rc != 0
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
        if made_src and not any(src_dir.iterdir()):
            src_dir.rmdir()


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
    p.add_argument("--crate-path", default="", help="the crate's directory inside a Cargo workspace")
    p.add_argument("--timeout", type=int, default=180)
    args = p.parse_args()

    fix_commit = args.fix_commit
    case_name = args.name
    category = None
    crate_path = args.crate_path.strip("/")
    if args.suite and args.name:
        suite = json.load(open(args.suite, encoding="utf-8"))
        match = next((c for c in suite.get("cases", []) if c["name"] == args.name), None)
        if not match:
            sys.exit(f"case `{args.name}` not found in {args.suite}")
        fix_commit = fix_commit or match.get("fix_commit")
        category = match.get("category")
        crate_path = crate_path or (match.get("crate_path") or "").strip("/")
        name = match.get("repo_name")
        if name and not (args.repo / ".git").exists() and (args.repo / name / ".git").exists():
            args.repo = args.repo / name
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
        "crate_path": crate_path or None,
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
        # One target directory for both commits: the dependencies build once.
        target_dir = pathlib.Path(tmp) / "cargo-target"
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

                on_parent = inject_and_test(wt_parent, src, args.timeout, crate_path, target_dir)
                entry["parent"] = on_parent
                if not on_parent["compiled"]:
                    status = "compile_error_on_parent"
                elif not on_parent["confirmed_marker"]:
                    status = "passed_on_parent"
                else:
                    on_fix = inject_and_test(wt_fix, src, args.timeout, crate_path, target_dir)
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
