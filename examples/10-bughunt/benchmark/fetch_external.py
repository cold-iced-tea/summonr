#!/usr/bin/env python3
"""Fetch the external bughunt suite: clone each third-party repo, pin its fix
commits, pre-fetch Cargo dependencies, and build the gold sets locally.

  fetch_external.py CACHE_DIR [--suite external-suite.json] [--gold-dir DIR]
                    [--repo NAME ...] [--case NAME ...] [--tier smoke|core|full]
                    [--no-cargo]

CACHE_DIR gets one full clone per `repo_name` (e.g. CACHE_DIR/http), and the
gold sets go to CACHE_DIR/gold unless --gold-dir says otherwise. Nothing
third-party is written into the Summonr tree.

For every selected case, both the fix commit and its parent are exported to a
scratch directory and `cargo fetch` is run there, so the registry cache holds
every dependency (dev-dependencies included) that `cargo test --offline`
needs in a lab at either commit. Rerunning is cheap: clones are reused and
only fetched again when a pinned commit is missing.

Then run a case, e.g.:
  setup.sh LAB CACHE_DIR/rust-url PARENT_COMMIT summonr url
  benchmark/run.sh CACHE_DIR OUT summonr --suite benchmark/external-suite.json \\
      --gold-dir CACHE_DIR/gold --tier smoke
"""
import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
TIERS = {"smoke": 0, "core": 1, "full": 2}


def run(cmd, cwd=None, quiet=True):
    p = subprocess.run(cmd, cwd=cwd, text=True, capture_output=quiet)
    if p.returncode != 0:
        detail = (p.stderr or p.stdout or "").strip() if quiet else ""
        sys.exit(f"failed: {' '.join(map(str, cmd))}\n{detail}")
    return (p.stdout or "").strip() if quiet else ""


def has_commit(repo, sha):
    return subprocess.run(["git", "-C", str(repo), "cat-file", "-e", f"{sha}^{{commit}}"],
                          capture_output=True).returncode == 0


def clone(cache, name, url, shas):
    dest = cache / name
    if not (dest / ".git").is_dir():
        print(f"[{name}] cloning {url}")
        # A full clone: partial clones would need the network again for the
        # old blobs that make_gold.py and the labs read.
        run(["git", "clone", "-q", url, str(dest)])
    else:
        origin = run(["git", "-C", str(dest), "remote", "get-url", "origin"])
        if origin.rstrip("/").removesuffix(".git") != url.rstrip("/").removesuffix(".git"):
            sys.exit(f"[{name}] {dest} is a clone of {origin}, not {url}")
    missing = [s for s in shas if not has_commit(dest, s)]
    if missing:
        print(f"[{name}] fetching {len(missing)} missing commit(s)")
        run(["git", "-C", str(dest), "fetch", "-q", "origin"])
        for s in missing:
            if not has_commit(dest, s):
                run(["git", "-C", str(dest), "fetch", "-q", "origin", s])
            if not has_commit(dest, s):
                sys.exit(f"[{name}] commit {s} not found upstream")
    return dest


def cargo_fetch(repo, rev, crate_path, scratch):
    """Export REV and fetch its dependencies into the shared Cargo cache."""
    out = scratch / rev
    out.mkdir(parents=True)
    archive = subprocess.Popen(["git", "-C", str(repo), "archive", rev], stdout=subprocess.PIPE)
    subprocess.run(["tar", "-x", "-C", str(out)], stdin=archive.stdout, check=True)
    if archive.wait() != 0:
        sys.exit(f"git archive {rev} failed in {repo}")
    manifest = out / (crate_path or ".") / "Cargo.toml"
    if not manifest.is_file():
        sys.exit(f"{repo.name}@{rev}: no Cargo.toml at {crate_path or '.'}")
    # From a workspace member cargo resolves the whole workspace, so the
    # lockfile it writes is the one a lab at this commit resolves offline.
    run(["cargo", "fetch", "-q", "--manifest-path", str(manifest)])
    shutil.rmtree(out)


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument("cache", type=pathlib.Path)
    p.add_argument("--suite", type=pathlib.Path, default=HERE / "external-suite.json")
    p.add_argument("--gold-dir", type=pathlib.Path)
    p.add_argument("--repo", action="append", default=[], help="only this repo_name (repeatable)")
    p.add_argument("--case", action="append", default=[], help="only this case name (repeatable)")
    p.add_argument("--tier", choices=sorted(TIERS, key=TIERS.get), help="this tier and below")
    p.add_argument("--no-cargo", action="store_true", help="skip pre-fetching Cargo dependencies")
    args = p.parse_args()

    suite = json.load(open(args.suite, encoding="utf-8"))
    cases = [c for c in suite["cases"]
             if (not args.repo or c["repo_name"] in args.repo)
             and (not args.case or c["name"] in args.case)
             and (not args.tier or TIERS[c.get("tier", "full")] <= TIERS[args.tier])]
    if not cases:
        sys.exit("no cases selected")
    cache = args.cache.expanduser().resolve()
    cache.mkdir(parents=True, exist_ok=True)
    gold_dir = (args.gold_dir or cache / "gold").expanduser().resolve()

    by_repo = {}
    for c in cases:
        by_repo.setdefault(c["repo_name"], []).append(c)
    clones = {}
    for name, group in by_repo.items():
        urls = {c["repo_url"] for c in group}
        if len(urls) != 1:
            sys.exit(f"{name}: cases disagree on repo_url: {sorted(urls)}")
        clones[name] = clone(cache, name, urls.pop(), [c["fix_commit"] for c in group])

    if not args.no_cargo:
        with tempfile.TemporaryDirectory(prefix="bughunt-fetch-") as tmp:
            for c in cases:
                repo = clones[c["repo_name"]]
                for rev in (f"{c['fix_commit']}^", c["fix_commit"]):
                    sha = run(["git", "-C", str(repo), "rev-parse", "--short=12", rev])
                    print(f"[{c['name']}] cargo fetch at {sha}")
                    cargo_fetch(repo, sha, c.get("crate_path"), pathlib.Path(tmp))

    # make_gold.py builds every case in the suite file it is given, so give
    # it only the selected ones.
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        json.dump({**suite, "cases": cases}, f)
    try:
        run([sys.executable, str(HERE / "make_gold.py"), "suite", str(cache), f.name, str(gold_dir)], quiet=False)
    finally:
        pathlib.Path(f.name).unlink()

    print(f"\n{'case':28} {'parent':12} {'hunks':>5}  files")
    for c in cases:
        g = json.load(open(gold_dir / c["gold"], encoding="utf-8"))
        print(f"{c['name']:28} {g['target']:12} {g['total']:>5}  {', '.join(g['by_file'])}")
    print(f"\nrepos in {cache}, gold sets in {gold_dir}")


if __name__ == "__main__":
    main()
