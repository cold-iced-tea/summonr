"""Read a rite start/show JSON receipt. Write one observation per line or its test patch."""
import argparse
import json
import sys


def export(record, patch):
    stages = {}
    for stage in record["stages"]:
        stage_id = stage["stage"]
        if stage_id in stages:
            raise ValueError(f"duplicate stage {stage_id}")
        stages[stage_id] = stage
    name = record["name"]
    if name == "flake-court":
        groups = [("probe", "evidence"), ("hearing", "comparison")]
    elif name == "report-to-test":
        groups = [("verify", "evidence")]
    else:
        raise ValueError(f"Unrecognized rite {name}")
    last = stages[groups[-1][0]]
    if not last["accepted"]:
        raise ValueError(f"{name} has no accepted receipt: {last['status']}")
    if patch:
        if name != "report-to-test":
            raise ValueError("Only Report to Test produces patches")
        return last["output"]["patch"]
    verdict = last["output"]["verdict"]
    lines = []
    for stage_name, key in groups:
        stage = stages[stage_name]
        evidence = stage["output"][key]
        for index, row in enumerate(evidence["rows"]):
            lines.append(json.dumps({
                **row,
                "rite": name, "rite_id": record["id"], "stage": stage_name,
                "summon": stage["summon"], "ritual_hash": stage["hash"], "ordinal": index + 1,
                "parent": evidence["parent"], "head": evidence["head"],
                "verdict": verdict
            }, sort_keys=True) + "\n")
    return "".join(lines)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--patch", action="store_true")
    args = parser.parse_args()
    try:
        envelope = json.load(sys.stdin)
        if envelope.get("ok") is False:
            raise ValueError(envelope.get("error"))
        sys.stdout.write(export(envelope.get("data", envelope), args.patch))
    except (KeyError, TypeError, ValueError) as exc:
        parser.exit(2, f"Invalid receipt: {exc}\n")
