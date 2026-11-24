#!/usr/bin/env python3
"""Build bughunt gold sets for bughunt-score.

Gold format: {"target": COMMIT, "source": ..., "total": N,
              "by_file": {FILE: [{"id": ..., "text": ...}]},
              ...optional metadata: fix_commit, category, tags, dirs, summary}

  make_gold.py fix REPO FIX_COMMIT [FIX_COMMIT...] [--category CAT] [--tags A,B] [--summary TEXT] [--paths src,crates] > gold.json
      One gold set per fix commit's parent is the honest unit; with several
      commits they must share a parent (otherwise run once per commit).
      Each hunk under --paths (default: src) the fix changed becomes a gold
      item whose text is the old side of the hunk: removed lines plus context.

  make_gold.py fix ... --crate-path crates/foo
      For a crate inside a Cargo workspace: --paths defaults to crates/foo/src.

  make_gold.py suite REPO SUITE_JSON OUT_DIR
      Generate all fix-commit gold files declared in SUITE_JSON into OUT_DIR.
      When a case names a `repo_name` (external-suite.json), REPO is the cache
      directory holding one clone per repo_name (see fetch_external.py). A
      case's `paths` default to `CRATE_PATH/src` or `src`.

  make_gold.py gemini REPO AUDIT_MD COMMIT [DIR...] > gold.json
      Each finding's file#Lx-Ly link in the audit markdown becomes a gold
      item whose text is those lines at COMMIT. Unverified claims. With DIRs
      (e.g. src/reagents), only files directly in them are kept, so recall
      is measured against what a hunt over those directories can see.
"""
import argparse
import os
import json
import pathlib
import re
import subprocess
import sys


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True, capture_output=True, text=True).stdout


def add(by_file, file, item):
    by_file.setdefault(file, []).append(item)


# A file's own unit-test module, where the fix's new tests go: `mod tests {`
# or `mod test {` at the top level of the file.
TEST_MODULE = re.compile(r"^mod tests? \{")


def is_test_file(file):
    name = file.rsplit("/", 1)[-1]
    parts = file.split("/")
    return (name.endswith("_tests.rs") or name == "tests.rs"
            or "tests" in parts[:-1] or "benches" in parts[:-1])


def default_paths(crate_path=None):
    return (f"{crate_path.strip('/')}/src",) if crate_path else ("src",)


def fix_gold(repo, commits, paths=("src",), category=None, tags=None, summary=None, extra=None):
    by_file, parents = {}, set()
    for c in commits:
        parent = git(repo, "rev-parse", "--short", f"{c}^").strip()
        parents.add(parent)
        diff = git(repo, "diff", "-U3", f"{c}^", c, "--", *paths)
        file, old, n, skip, tests_at = None, [], 0, False, None

        def flush():
            nonlocal old, n
            text = "\n".join(l.strip() for l in old if l.strip())
            if file and text:
                n += 1
                add(by_file, file, {"id": f"{c}:{file}:{n}", "text": text})
            old = []

        for line in diff.splitlines():
            if line.startswith("--- "):
                flush()
                file = line[6:] if line.startswith("--- a/") else None
                if file and is_test_file(file):
                    file = None  # test code is not where a bug lives
                tests_at = None
                if file:
                    src = git(repo, "show", f"{c}^:{file}").splitlines()
                    tests_at = next((i + 1 for i, l in enumerate(src) if TEST_MODULE.match(l)), None)
            elif line.startswith("+++ "):
                continue
            elif line.startswith("@@"):
                flush()
                start = int(re.match(r"@@ -(\d+)", line).group(1))
                # Hunks inside the file's test module are the fix's new tests,
                # not where the bug was.
                skip = tests_at is not None and start >= tests_at
            elif skip:
                continue
            elif line.startswith("-") or line.startswith(" "):
                old.append(line[1:])
        flush()
    if len(parents) != 1:
        sys.exit(f"fix commits have different parents {sorted(parents)}; make one gold set per parent")
    total = sum(len(v) for v in by_file.values())
    dirs = sorted({f.rsplit("/", 1)[0] for f in by_file if "/" in f})
    out = {
        "target": parents.pop(),
        "source": "fix commits " + " ".join(commits),
        "total": total,
        "by_file": by_file,
    }
    if len(commits) == 1:
        out["fix_commit"] = commits[0]
    if dirs:
        out["dirs"] = dirs
    if category:
        out["category"] = category
    if tags:
        out["tags"] = list(tags)
    if summary:
        out["summary"] = summary
    for k, v in (extra or {}).items():
        if v:
            out[k] = v
    return out


def gemini_gold(repo, audit, commit, dirs=()):
    by_file, seen = {}, set()
    link = re.compile(r"file://[^)#]*?/(src/[^)#]+)#L(\d+)(?:-L(\d+))?")
    current = None
    for line in open(audit, encoding="utf-8"):
        m = re.match(r"\s*(\d+)\.\s+\*\*", line) or re.match(r"\|\s*(\d+)\s*\|", line)
        if m:
            current = m.group(1)
        for f, a, b in link.findall(line):
            if dirs and f.rsplit("/", 1)[0] not in dirs:
                continue
            key = (current, f, a)
            if key in seen:
                continue
            seen.add(key)
            lines = git(repo, "show", f"{commit}:{f}").splitlines()
            lo, hi = int(a), int(b or a)
            text = "\n".join(l.strip() for l in lines[lo - 1:hi] if l.strip())
            if text:
                add(by_file, f, {"id": f"gemini-{current}:{f}:{a}", "text": text})
    total = sum(len(v) for v in by_file.values())
    return {"target": commit, "source": "gemini audit (unverified)" + (f" in {', '.join(dirs)}" if dirs else ""), "total": total, "by_file": by_file}


def build_suite(repo, suite_path, out_dir):
    suite = json.load(open(suite_path, encoding="utf-8"))
    out_dir = pathlib.Path(out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    built = 0
    for case in suite.get("cases", []):
        fix_commit = case.get("fix_commit")
        if not fix_commit:
            continue
        crate_path = case.get("crate_path")
        paths = tuple(case.get("paths") or default_paths(crate_path))
        case_repo = repo
        if case.get("repo_name"):
            case_repo = os.path.join(repo, case["repo_name"])
            if not os.path.isdir(os.path.join(case_repo, ".git")):
                sys.exit(f"{case['name']}: no clone of {case['repo_name']} in {repo} (run fetch_external.py)")
        gold = fix_gold(
            case_repo,
            [fix_commit],
            paths=paths,
            category=case.get("category"),
            tags=case.get("tags"),
            summary=case.get("summary"),
            extra={k: case.get(k) for k in ("repo_name", "repo_url", "crate_path")},
        )
        if not gold["total"]:
            sys.exit(f"{case['name']}: the fix changed no non-test lines under {', '.join(paths)}")
        dest = out_dir / case["gold"]
        dest.write_text(json.dumps(gold, indent=1) + "\n", encoding="utf-8")
        built += 1
    print(f"Generated {built} fix-commit gold files in {out_dir}")


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    mode = sys.argv[1]
    if mode == "fix":
        p = argparse.ArgumentParser(prog="make_gold.py fix")
        p.add_argument("repo")
        p.add_argument("commits", nargs="+")
        p.add_argument("--category")
        p.add_argument("--tags", help="comma-separated tags")
        p.add_argument("--summary")
        p.add_argument("--paths", help="comma-separated source paths (default: src, or CRATE_PATH/src)")
        p.add_argument("--crate-path", help="the crate's directory inside a Cargo workspace, e.g. crates/foo")
        args = p.parse_args(sys.argv[2:])
        tags = [t.strip() for t in args.tags.split(",") if t.strip()] if args.tags else None
        paths = tuple(x.strip() for x in (args.paths or "").split(",") if x.strip()) or default_paths(args.crate_path)
        out = fix_gold(args.repo, args.commits, paths=paths, category=args.category, tags=tags, summary=args.summary,
                       extra={"crate_path": args.crate_path})
        json.dump(out, sys.stdout, indent=1)
        print()
    elif mode == "suite":
        if len(sys.argv) != 5:
            sys.exit("usage: make_gold.py suite REPO SUITE_JSON OUT_DIR")
        build_suite(sys.argv[2], sys.argv[3], sys.argv[4])
    elif mode == "gemini":
        if len(sys.argv) < 5:
            sys.exit(__doc__)
        out = gemini_gold(sys.argv[2], sys.argv[3], sys.argv[4], tuple(sys.argv[5:]))
        json.dump(out, sys.stdout, indent=1)
        print()
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
