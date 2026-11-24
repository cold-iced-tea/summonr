"""Workers for the two local testing rites. Only run trusted project tests."""
import argparse
import difflib
import hashlib
import linecache
import json
import keyword
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

HERE = Path(__file__).resolve()
BUDGET = 8


def git(*args, cwd=None):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                          text=True, timeout=60).stdout.strip()


def identifier(name):
    return isinstance(name, str) and name.isascii() and name.isidentifier() and not keyword.iskeyword(name)


def dotted(name):
    return isinstance(name, str) and all(identifier(part) for part in name.split("."))


def patch_for(report):
    module, function = report["module"], report["function"]
    if not dotted(module) or not identifier(function):
        raise ValueError("Expected a dotted module and a function name")
    if not isinstance(report["args"], list):
        raise ValueError("Report args must be a JSON array")
    title = report["title"]
    # Use the same literal source in the patch and both executions.
    source = (
        "import importlib\nimport unittest\n\n\n"
        "class ReportRegression(unittest.TestCase):\n"
        "    def test_report(self):\n"
        f"        {title!r}\n"
        f"        function = getattr(importlib.import_module({module!r}), {function!r})\n"
        f"        actual = function(*{report['args']!r})\n"
        f"        self.assertEqual(actual, {report['expected']!r}, 'reported result differs')\n"
    )
    digest = hashlib.sha256(source.encode()).hexdigest()
    filename = f"test_report_{digest[:12]}.py"
    patch = "".join(difflib.unified_diff([], source.splitlines(True),
                                        fromfile="/dev/null", tofile=f"b/{filename}"))
    return filename, source, patch, digest


class Observation(unittest.TestResult):
    def __init__(self):
        super().__init__()
        self.rows = []

    def record(self, test, status, detail=""):
        self.rows.append({"test": test.id(), "status": status, "detail": detail[:180]})

    def addSuccess(self, test):
        self.record(test, "pass")

    def addFailure(self, test, err):
        # A generated test must reach its comparison. An AssertionError thrown
        # by the function under test is an execution error for the report.
        tb = err[2]
        at_comparison = False
        while tb:
            if tb.tb_frame.f_code.co_name == "test_report" and linecache.getline(tb.tb_frame.f_code.co_filename, tb.tb_lineno).lstrip().startswith("self.assertEqual(actual,"):
                at_comparison = True
            tb = tb.tb_next
        self.record(test, "fail", str(err[1]))
        self.rows[-1]["comparison"] = at_comparison

    def addError(self, test, err):
        self.record(test, "error", f"{err[0].__name__}: {err[1]}")

    def addSkip(self, test, reason):
        self.record(test, "skipped", reason)

    def addExpectedFailure(self, test, err):
        self.record(test, "expected_failure")

    def addUnexpectedSuccess(self, test):
        self.record(test, "unexpected_success")

    def addSubTest(self, test, subtest, err):
        if err is not None:
            self.record(test, "subtest_failure", str(err[1]))


def worker(names, sink):
    sys.path.insert(0, os.getcwd())
    loader = unittest.TestLoader()
    suite = loader.loadTestsFromNames(names)
    result = Observation()
    suite.run(result)
    Path(sink).write_text(json.dumps(result.rows))


def observe(spec, rev, names, source=None):
    start = time.monotonic()
    row = {"revision": rev, "sequence": names}
    with tempfile.TemporaryDirectory() as tmp:
        checkout = Path(tmp) / "checkout"
        sink = Path(tmp) / "result.json"
        git("clone", "--quiet", "--no-checkout", "--no-hardlinks", "--", spec["repo"], str(checkout))
        git("checkout", "--quiet", "--detach", rev, cwd=checkout)
        if source:
            filename, body = source
            with (checkout / filename).open("x") as f:
                f.write(body)
        with subprocess.Popen([sys.executable, "-B", str(HERE), "_worker", str(sink), *names],
                              cwd=checkout, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                              start_new_session=True) as proc:
            try:
                proc.wait(timeout=spec["timeout"])
            except subprocess.TimeoutExpired:
                row["status"] = "timeout"
            finally:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        if "status" not in row:
            results = json.loads(sink.read_text()) if sink.exists() and proc.returncode == 0 else []
            row["results"] = results[:len(names)]
            row["status"] = "error"
            if ([r["test"] for r in results] == names and
                    all(r["status"] == "pass" for r in results[:-1])):
                row["status"] = results[-1]["status"]
    row["ms"] = round((time.monotonic() - start) * 1000)
    return row


def probe(spec):
    rows = [observe(spec, spec["head"], [spec["target"]]) for _ in range(2)]
    statuses = [r["status"] for r in rows]
    if statuses == ["pass", "pass"]:
        plan = "order"
    elif statuses == ["fail", "fail"]:
        plan = "revision"
    elif set(statuses) == {"pass", "fail"}:
        plan = "repeat"
    else:
        plan = "stop"
    return {"plan": plan, "rows": rows, "budget": BUDGET}


def challenge(spec, plan):
    target = [spec["target"]]
    pair = [spec["predecessor"], *target]
    rows = []

    def twice(ref, seq):
        batch = [observe(spec, spec[ref], seq) for _ in range(2)]
        rows.extend(batch)
        return [r["status"] for r in batch]

    verdict = "inconclusive"
    if plan == "order":
        sequence = twice("head", pair)
        if sequence == ["fail", "fail"]:
            verdict = "order_sensitivity_observed"
            alone = twice("parent", target)
            ordered = twice("parent", pair)
            if alone == ["pass", "pass"] and ordered == sequence:
                verdict = "order_sensitivity_on_both_revisions"
            elif alone == ordered == ["pass", "pass"]:
                verdict = "order_sensitivity_only_observed_on_head"
        elif sequence == ["pass", "pass"]:
            verdict = "failure_not_observed"
        elif set(sequence) == {"pass", "fail"}:
            verdict = "mixed_ordered_results"
    elif plan == "revision":
        parent = twice("parent", target)
        if parent == ["pass", "pass"]:
            verdict = "isolated_failure_only_observed_on_head"
        elif parent == ["fail", "fail"]:
            verdict = "isolated_failure_on_both_revisions"
    elif plan == "repeat":
        twice("head", target)
        verdict = "mixed_isolated_results"
    elif plan != "stop":
        raise ValueError("Unknown probe plan")
    return {"verdict": verdict, "rows": rows, "runs_used": 2 + len(rows),
            "budget_remaining": BUDGET - 2 - len(rows)}


def verify(spec, digest):
    filename, source, _, actual = patch_for(spec["report"])
    if actual != digest:
        raise ValueError("Draft changed after preparation")
    name = filename[:-3] + ".ReportRegression.test_report"
    rows = [observe(spec, spec[ref], [name], (filename, source)) for ref in ("parent", "head")]
    parent, fixed = rows
    verdict = "inconclusive"
    if parent["status"] == fixed["status"] == "pass":
        verdict = "already_passed"
    elif parent["status"] == "fail" and parent["results"][-1].get("comparison"):
        if fixed["status"] == "pass":
            verdict = "verified_fix"
        elif fixed["status"] == "fail" and fixed["results"][-1].get("comparison"):
            verdict = "still_fails"
    return {"verdict": verdict, "test_sha256": digest, "rows": rows}


def main():
    if sys.argv[1:2] == ["_worker"]:
        worker(sys.argv[3:], sys.argv[2])
        return
    parser = argparse.ArgumentParser()
    parser.add_argument("spec", type=Path)
    parser.add_argument("action", choices=["probe", "challenge", "draft", "verify"])
    parser.add_argument("argument", nargs="?")
    args = parser.parse_args()
    raw = args.spec.read_bytes()
    if args.spec.name != "case-" + hashlib.sha256(raw).hexdigest() + ".json":
        raise ValueError("Case manifest changed since preparation")
    spec = json.loads(raw)
    if args.action == "probe":
        result = probe(spec)
    elif args.action == "challenge":
        result = challenge(spec, args.argument)
    elif args.action == "draft":
        _, _, patch, digest = patch_for(spec["report"])
        result = {"report": spec["report"]["title"], "patch": patch, "sha256": digest}
    else:
        result = verify(spec, args.argument)
    result.update(parent=spec["parent"], head=spec["head"])
    print(json.dumps(result, allow_nan=False))


if __name__ == "__main__":
    main()
