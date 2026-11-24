#!/usr/bin/env python3
"""Aggregate bughunt benchmark results by case and by bug category.

Usage:
  report.py OUT_DIR [--suite SUITE_JSON] [--markdown]
"""
import argparse
import collections
import json
import pathlib


def load_json(path: pathlib.Path):
    if not path.exists():
        return None
    try:
        return json.load(open(path, encoding="utf-8"))
    except Exception:
        return None


def main():
    here = pathlib.Path(__file__).resolve().parent
    p = argparse.ArgumentParser(description="Report bughunt benchmark results by case and category.")
    p.add_argument("out_dir", type=pathlib.Path)
    p.add_argument("--suite", type=pathlib.Path, default=here / "suite.json")
    p.add_argument("--markdown", action="store_true")
    args = p.parse_args()

    suite = load_json(args.suite) or {"cases": []}
    by_name = {c["name"]: c for c in suite.get("cases", [])}

    rows = []
    cat_stats = collections.defaultdict(
        lambda: {
            "cases": 0,
            "gold_total": 0,
            "raw_hit": 0,
            "gathered_hit": 0,
            "gathered_findings": 0,
            "requests": 0,
            "verified_fix": 0,
            "confirmed_input": 0,
        }
    )

    seen_names = []
    for c in suite.get("cases", []):
        if (args.out_dir / f"bench-{c['name']}.json").exists() or (args.out_dir / f"verify-{c['name']}.json").exists():
            seen_names.append(c["name"])
    for f in sorted(args.out_dir.glob("bench-*.json")):
        name = f.stem[len("bench-"):]
        if name not in seen_names:
            seen_names.append(name)

    for name in seen_names:
        meta = by_name.get(name, {})
        category = meta.get("category", "uncategorized")
        bench = load_json(args.out_dir / f"bench-{name}.json") or {}
        verify = load_json(args.out_dir / f"verify-{name}.json") or {}
        res = bench.get("result") or {}
        raw = res.get("raw") or {}
        kept = res.get("after_gather") or {}
        gt = int(raw.get("gold_total") or 0)
        rh = int(raw.get("gold_hit") or 0)
        gh = int(kept.get("gold_hit") or 0)
        gf = int(kept.get("findings") or 0)
        req = int(bench.get("requests") or 0)
        vf = int((verify.get("counts") or {}).get("verified_fix") or 0)
        ci = int(verify.get("confirmed_input") or 0)
        rows.append(
            {
                "name": name,
                "category": category,
                "outcome": bench.get("outcome", "-"),
                "gold_total": gt,
                "raw_hit": rh,
                "gathered_hit": gh,
                "gathered_findings": gf,
                "requests": req,
                "verified_fix": vf,
                "confirmed_input": ci,
            }
        )
        cs = cat_stats[category]
        cs["cases"] += 1
        cs["gold_total"] += gt
        cs["raw_hit"] += rh
        cs["gathered_hit"] += gh
        cs["gathered_findings"] += gf
        cs["requests"] += req
        cs["verified_fix"] += vf
        cs["confirmed_input"] += ci

    if not rows:
        print(f"No bench-*.json or verify-*.json files found in {args.out_dir}")
        return

    def pct(hit, total):
        return f"{(100.0 * hit / total):.0f}%" if total else "-"

    if args.markdown:
        print("### Per-Case Results\n")
        print("| Case | Category | Outcome | Gold | Raw Hit | Gathered Hit | Findings | Verified Fix |")
        print("|---|---|---|---:|---:|---:|---:|---:|")
        for r in rows:
            vf_str = f"{r['verified_fix']}/{r['confirmed_input']}" if r["confirmed_input"] else "-"
            print(
                f"| `{r['name']}` | `{r['category']}` | {r['outcome']} | {r['gold_total']} | "
                f"{r['raw_hit']}/{r['gold_total']} ({pct(r['raw_hit'], r['gold_total'])}) | "
                f"{r['gathered_hit']}/{r['gold_total']} ({pct(r['gathered_hit'], r['gold_total'])}) | "
                f"{r['gathered_findings']} | {vf_str} |"
            )
        print("\n### Recall & Verification by Category\n")
        print("| Category | Cases | Gold | Raw Recall | Gathered Recall | Findings | Verified Fixes |")
        print("|---|---:|---:|---:|---:|---:|---:|")
        for cat in sorted(cat_stats):
            s = cat_stats[cat]
            vf_str = f"{s['verified_fix']}/{s['confirmed_input']}" if s["confirmed_input"] else "-"
            print(
                f"| `{cat}` | {s['cases']} | {s['gold_total']} | "
                f"{s['raw_hit']}/{s['gold_total']} ({pct(s['raw_hit'], s['gold_total'])}) | "
                f"{s['gathered_hit']}/{s['gold_total']} ({pct(s['gathered_hit'], s['gold_total'])}) | "
                f"{s['gathered_findings']} | {vf_str} |"
            )
    else:
        print(f"{'CASE':20s} {'CATEGORY':17s} {'OUTCOME':9s} {'RAW':11s} {'GATHERED':11s} {'FIND':5s} {'VERIFIED':9s}")
        for r in rows:
            raw_s = f"{r['raw_hit']}/{r['gold_total']} ({pct(r['raw_hit'], r['gold_total'])})"
            gat_s = f"{r['gathered_hit']}/{r['gold_total']} ({pct(r['gathered_hit'], r['gold_total'])})"
            vf_s = f"{r['verified_fix']}/{r['confirmed_input']}" if r["confirmed_input"] else "-"
            print(f"{r['name']:20s} {r['category']:17s} {r['outcome']:9s} {raw_s:11s} {gat_s:11s} {r['gathered_findings']:5d} {vf_s:9s}")
        print("\nBY CATEGORY:")
        print(f"{'CATEGORY':17s} {'CASES':5s} {'RAW':12s} {'GATHERED':12s} {'FIND':5s} {'VERIFIED':9s}")
        tot_g = tot_r = tot_k = tot_f = tot_v = tot_c = 0
        for cat in sorted(cat_stats):
            s = cat_stats[cat]
            tot_g += s["gold_total"]
            tot_r += s["raw_hit"]
            tot_k += s["gathered_hit"]
            tot_f += s["gathered_findings"]
            tot_v += s["verified_fix"]
            tot_c += s["confirmed_input"]
            raw_s = f"{s['raw_hit']}/{s['gold_total']} ({pct(s['raw_hit'], s['gold_total'])})"
            gat_s = f"{s['gathered_hit']}/{s['gold_total']} ({pct(s['gathered_hit'], s['gold_total'])})"
            vf_s = f"{s['verified_fix']}/{s['confirmed_input']}" if s["confirmed_input"] else "-"
            print(f"{cat:17s} {s['cases']:5d} {raw_s:12s} {gat_s:12s} {s['gathered_findings']:5d} {vf_s:9s}")
        raw_t = f"{tot_r}/{tot_g} ({pct(tot_r, tot_g)})"
        gat_t = f"{tot_k}/{tot_g} ({pct(tot_k, tot_g)})"
        vf_t = f"{tot_v}/{tot_c}" if tot_c else "-"
        print(f"{'TOTAL':17s} {len(rows):5d} {raw_t:12s} {gat_t:12s} {tot_f:5d} {vf_t:9s}")


if __name__ == "__main__":
    main()
