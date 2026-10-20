"""Retained pilot arithmetic only; no workload or build execution."""
import datetime
import json
import pathlib
import statistics
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import collect

root = HERE / "finalized-v1/results-01"
report = {"kind": "five-block-pilot-descriptive-only", "samples": 0, "failures": 0, "censored": 0, "cells": [],
          "metric": "existing driver invoke_ns, divided by the same per-cell repeat count; process wall is separate",
          "ratio": "within matched block: arm invoke_ns / B invoke_ns and / preceding stage invoke_ns; >1 means longer elapsed invocation",
          "uncertainty": "Five position-balanced blocks with fresh processes; observed range is descriptive, not a confidence interval or adoption judgment."}
def stats(values):
    return {"median": statistics.median(values), "min": min(values), "max": max(values)}

for directory in sorted(p for p in root.iterdir() if p.is_dir()):
    completion = json.loads((directory / "completion.json").read_text())
    if completion["state"] != "complete" or completion["failed_arms"] or len(completion["completed"]) != 25:
        raise ValueError("incomplete/failed cell: " + directory.name)
    rows = []
    for path in sorted(directory.glob("block-*/result.json")):
        row = json.loads(path.read_text())
        driver = json.loads((path.parent / "stdout.bin").read_text())
        if row["status"] != "success" or row["returncode"] != 0 or row["censored"] or row["oracle_errors"]:
            raise ValueError("invalid row: " + str(path))
        for stream in ("stdout", "stderr"):
            observed = path.parent / (stream + ".bin")
            if collect.digest(observed) != row[stream]["sha256"] or observed.stat().st_size != row[stream]["bytes"]:
                raise ValueError("raw stream changed")
        if row["stderr"]["bytes"] or row["stack_limit"] != [8388608, -1] or row["timeout_seconds"] != 60 or row["max_output_bytes"] != 1048576:
            raise ValueError("policy mismatch")
        errors = collect.driver_errors({"kind": "primary", "input": row["expected_input"], "backend": row["backend"]}, row["arm"], driver)
        if errors or driver != row["driver_phases"]:
            raise ValueError(errors or "driver/raw output mismatch")
        rows.append({"arm": row["arm"]["id"], "block": row["block"], "position": row["position"],
                     "invoke_ns": driver["invoke_ns"], "repeat": driver["repeat"], "process_wall_ns": row["wall_ns"],
                     "phase_ns": {k: driver[k] for k in ("package_ns", "construct_ns", "warmup_ns", "release_roots_ns", "total_phases_ns")},
                     "result_file": str(path), "result_sha256": collect.digest(path)})
    if len(rows) != 25 or len({(r["arm"], r["block"]) for r in rows}) != 25:
        raise ValueError("unexpected matched-block sample set")
    cell = {"cell": directory.name, "samples": 25, "failures": 0, "rows": rows, "arms": {}}
    lookup = {(r["block"], r["arm"]): r for r in rows}
    for i, arm in enumerate(collect.ARMS):
        selected = sorted((r for r in rows if r["arm"] == arm), key=lambda r: r["block"])
        repeat = selected[0]["repeat"]
        if any(r["repeat"] != repeat for r in rows):
            raise ValueError("unequal work count")
        baseline = [r["invoke_ns"] / lookup[(r["block"], "B")]["invoke_ns"] for r in selected]
        previous = [r["invoke_ns"] / lookup[(r["block"], collect.ARMS[i - 1])]["invoke_ns"] for r in selected] if i else None
        cell["arms"][arm] = {"repeat": repeat, "invoke_ns": stats([r["invoke_ns"] for r in selected]),
                             "ns_per_repeat": stats([r["invoke_ns"] / repeat for r in selected]),
                             "outer_process_wall_ns": stats([r["process_wall_ns"] for r in selected]),
                             "paired_to_B": {**stats(baseline), "matched_block_ratios": baseline},
                             "paired_to_previous": {**stats(previous), "matched_block_ratios": previous} if previous else None}
    report["cells"].append(cell)
    report["samples"] += len(rows)
start = datetime.datetime.fromisoformat((HERE / "pilot-start").read_text().strip().replace("Z", "+00:00"))
end = datetime.datetime.fromisoformat((HERE / "pilot-end").read_text().strip().replace("Z", "+00:00"))
report.update(start=start.isoformat(), end=end.isoformat(), orchestration_elapsed_seconds=(end - start).total_seconds())
collect.save(HERE / "pilot-analysis.json", report)
print("cell arm median_ns_per_repeat range_ns_per_repeat paired_B_median paired_previous_median")
for cell in report["cells"]:
    for arm, value in cell["arms"].items():
        s = value["ns_per_repeat"]
        previous = value["paired_to_previous"]
        print(cell["cell"], arm, round(s["median"], 3), f"{s['min']:.3f}..{s['max']:.3f}",
              round(value["paired_to_B"]["median"], 5), round(previous["median"], 5) if previous else "-")
print("samples", report["samples"], "failures", report["failures"], "censored", report["censored"], "elapsed_seconds", report["orchestration_elapsed_seconds"])
