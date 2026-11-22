#!/usr/bin/env python3
"""Build bughunt gold sets for bughunt-score.

Gold format: {"target": COMMIT, "source": ..., "total": N,
              "by_file": {FILE: [{"id": ..., "text": ...}]}}

  make_gold.py fix REPO FIX_COMMIT [FIX_COMMIT...] > gold.json
      One gold set per fix commit's parent is the honest unit; with several
      commits they must share a parent (otherwise run once per commit).
      Each src/ hunk the fix changed becomes a gold item whose text is the
      old side of the hunk: removed lines plus surrounding context.

  make_gold.py gemini REPO AUDIT_MD COMMIT > gold.json
      Each finding's file#Lx-Ly link in the audit markdown becomes a gold
      item whose text is those lines at COMMIT. Unverified claims.
"""
import json
import re
import subprocess
import sys


def git(repo, *args):
    return subprocess.run(["git", "-C", repo, *args], check=True, capture_output=True, text=True).stdout


def add(by_file, file, item):
    by_file.setdefault(file, []).append(item)


def fix_gold(repo, commits):
    by_file, parents = {}, set()
    for c in commits:
        parent = git(repo, "rev-parse", "--short", f"{c}^").strip()
        parents.add(parent)
        diff = git(repo, "diff", "-U3", f"{c}^", c, "--", "src")
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
                if file and (file.endswith("_tests.rs") or "/tests/" in file):
                    file = None  # test code is not where a bug lives
                tests_at = None
                if file:
                    src = git(repo, "show", f"{c}^:{file}").splitlines()
                    tests_at = next((i + 1 for i, l in enumerate(src) if l.startswith("mod tests {")), None)
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
    return {"target": parents.pop(), "source": "fix commits " + " ".join(commits), "total": total, "by_file": by_file}


def gemini_gold(repo, audit, commit):
    by_file, seen = {}, set()
    link = re.compile(r"file://[^)#]*?/(src/[^)#]+)#L(\d+)(?:-L(\d+))?")
    current = None
    for line in open(audit, encoding="utf-8"):
        m = re.match(r"\s*(\d+)\.\s+\*\*", line) or re.match(r"\|\s*(\d+)\s*\|", line)
        if m:
            current = m.group(1)
        for f, a, b in link.findall(line):
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
    return {"target": commit, "source": "gemini audit (unverified)", "total": total, "by_file": by_file}


def main():
    if len(sys.argv) >= 4 and sys.argv[1] == "fix":
        out = fix_gold(sys.argv[2], sys.argv[3:])
    elif len(sys.argv) == 5 and sys.argv[1] == "gemini":
        out = gemini_gold(sys.argv[2], sys.argv[3], sys.argv[4])
    else:
        sys.exit(__doc__)
    json.dump(out, sys.stdout, indent=1)
    print()


if __name__ == "__main__":
    main()
