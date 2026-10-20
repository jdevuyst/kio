"""Descriptive summaries of the fixed completed compiler panel; no workloads."""
import collections
import json
import pathlib
import re
import statistics
import sys
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import collect

d = json.loads((HERE / "results-01/completion.json").read_text())
assert d["state"] == "complete" and len(d["completed"]) == 180 and not d["unattempted"]
groups = collections.defaultdict(dict)
raw = []
for record in d["completed"]:
    assert record["success"]
    row = record["row"]
    folder = pathlib.Path(record["process"]).parent
    p = json.loads((folder / "process.json").read_text())
    owner = json.loads((folder / "ownership.json").read_text())
    assert p["returncode"] == 0 and not p["censored"] and owner == {"interrupted": False, "worker_returncode": 0}
    for stream in ("stdout", "stderr"):
        assert collect.digest(p[stream]["path"]) == p[stream]["sha256"]
    cell, arm = row["command"].split("/")
    block = int(re.search(r"-b([123])-p", row["id"]).group(1))
    inventory = json.loads((folder.parent / "retained-file-inventory.json").read_text())
    generated = [f for f in inventory["files"] if "/out/" in f["path"]]
    value = {"cell": cell, "arm": arm, "block": block, "wall_ns": p["wall_ns"],
        "child_cpu_seconds": p["child_user_cpu_seconds"] + p["child_system_cpu_seconds"],
        "max_child_rss_bytes": p["max_child_rss_bytes"],
        "emitted_bytes": sum(f["bytes"] for f in generated), "emitted_files": len(generated),
        "stdout_bytes": p["stdout"]["bytes"], "stderr_bytes": p["stderr"]["bytes"],
        "process": str(folder / "process.json"), "process_sha256": collect.digest(folder / "process.json")}
    raw.append(value)
    groups[cell].setdefault(arm, {})[block] = value

def summary(values):
    return {"median": statistics.median(values), "min": min(values), "max": max(values)}

cells = {}
lines = ["Descriptive three-block compiler panel. Outer scheduled-command elapsed, not isolated compiler-pass cost.",
         "Three rotating matched blocks have incomplete five-position balance; ranges are not confidence intervals."]
for cell, arms in groups.items():
    rows = {}
    for i, arm in enumerate(("B", "T", "C", "S", "P")):
        values = [arms[arm][b] for b in (1, 2, 3)]
        row = {metric: summary([v[metric] for v in values]) for metric in
               ("wall_ns", "child_cpu_seconds", "max_child_rss_bytes", "emitted_bytes", "emitted_files")}
        if arm != "B":
            for label, reference in (("B", "B"), ("previous", ("B", "T", "C", "S", "P")[i-1])):
                row["matched_wall_ratio_to_" + label] = summary([arms[arm][b]["wall_ns"] / arms[reference][b]["wall_ns"] for b in (1,2,3)])
        rows[arm] = row
        lines.append(f"{cell:20} {arm} wall_ms median/min/max=" + "/".join(f"{row['wall_ns'][k]/1e6:.3f}" for k in ("median","min","max")) +
            f" cpu_s={row['child_cpu_seconds']['median']:.4f} max_child_rss_mib={row['max_child_rss_bytes']['median']/1048576:.2f} emitted_bytes={row['emitted_bytes']['median']}" +
            (f" paired/B={row['matched_wall_ratio_to_B']['median']:.4f} paired/previous={row['matched_wall_ratio_to_previous']['median']:.4f}" if arm != "B" else ""))
    cells[cell] = rows
collect.save(HERE / "results-01/analysis.json", {"scope": lines[:2], "completion_sha256": collect.digest(HERE / "results-01/completion.json"),
    "cells": cells, "rows": raw})
print("\n".join(lines))
