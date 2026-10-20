"""Read retained application samples; existing matched-block arithmetic only."""
import hashlib
import json
import pathlib
import statistics
import sys

HERE = pathlib.Path(__file__).resolve().parent
PRIMARY = HERE.parent.parent
sys.path.insert(0, str(PRIMARY))
import collect
from prepare import schedule


def read(path):
    return json.loads(pathlib.Path(path).read_text())


def pin(path):
    return dict(path=str(path), sha256=collect.digest(path))


def stats(values):
    return dict(n=len(values), median=statistics.median(values), min=min(values), max=max(values))


def main():
    manifest = read(HERE / "manifest.json")
    root = HERE / "results-01"
    completion = read(root / "completion.json")
    assert completion["state"] == "complete" and not completion["unattempted_cells"] and not completion["drain_error"]
    assert collect.digest(root / "completion.json") == "3a2cef03354903d42b5b09054e0684ec73f7e7c0f505be4a22ef1e52220d6c22"
    if sys.argv[1] == "meta":
        print(json.dumps(dict(kind="descriptive-application-process-and-repeated-entry", samples=275, failures=[], censored=0,
            cells=[r["cell"] for r in manifest["cells"]],
            sources=[pin(p) for p in (HERE / "manifest.json", root / "completion.json", root / "launch.json", HERE.parent / "APPLICATION-PROTOCOL.md", HERE.parent / "run_application.py", pathlib.Path(__file__), PRIMARY / "collect.py")],
            uncertainty="Matched block min/median/max only, no confidence intervals, independence or adoption claim.",
            ratio="metric(stage)/metric(B or preceding stage) within the same block; greater than1 is longer elapsed time",
            limits=["Six process cells include startup/load/host/entry/IO cost; no subtraction or invocation-time claim.",
                "JS uses default-dev-opt0 public Rust launcher and embedded rquickjs0.11; C-engine O0 is recipe-inferred, not observed original C argv. Not optimized-production JS or primary isolated release-opt3 invocation.",
                "Python uses default-dev Rust launcher and pinned external Python3.12.15. Rust guest/driver application binaries are opt2/debug0. No cross-profile pooling.",
                "Repeated-entry is fixed19-event entry with memory print sink,1000 measured repeats/warmup0; not original stdout IO, isolated matching-kernel throughput or proven steady state.",
                "Repeated invoke includes guest entry, Book/arrivals reset, domain work, formatting and memory appends; loop additionally includes buffer clear/full-byte check/checksum. One shared package is created before both intervals; outer wall is separate.",
                "All2509 expected bytes checked per iteration;2,509,000 bytes per repeated sample. No allocation/heap/collection inference; root-release and coarse child RSS do not isolate capture destruction or target-only memory."])))
        return
    cell = next(r for r in manifest["cells"] if r["cell"] == sys.argv[1])
    assert collect.digest(cell["path"]) == cell["sha256"]
    doc = read(cell["path"])
    repeated = cell["kind"] == "repeated-entry"
    directory = root / cell["cell"]
    if not repeated:
        done = read(directory / "completion.json")
        assert done["state"] == "complete" and not done["failed_arms"] and len(done["completed"]) == cell["samples"]
    if repeated:
        oracle = pathlib.Path(doc["oracle"]["path"]).read_bytes()
        assert len(oracle) == 2509 and collect.digest(doc["oracle"]["path"]) == doc["oracle"]["sha256"]
        checksum = 14695981039346656037
        for byte in oracle * 1000:
            checksum = ((checksum ^ byte) * 1099511628211) & ((1 << 64) - 1)
        assert f"{checksum:016x}" == doc["expected_fnv1a64"]
    rows = []
    identity = []
    for path in sorted(directory.glob("block-*/result.json")):
        row = read(path)
        arm = row["arm"]["id"]
        assert row["status"] == "success" and row["returncode"] == 0 and not row["censored"] and not row["oracle_errors"]
        assert row["trusted_command_sha256"] == cell["sha256"]
        assert row["stack_limit"] == [8388608, -1] and row["timeout_seconds"] == 60 and row["max_output_bytes"] == 1048576
        assert schedule(doc["schedule_seed"])[row["block"]][row["position"]] == arm
        assert row["arm"] == next(a for a in doc["arms"] if a["id"] == arm)
        for stream in ("stdout", "stderr"):
            p = path.parent / (stream + ".bin")
            assert collect.digest(p) == row[stream]["sha256"] and p.stat().st_size == row[stream]["bytes"]
        metrics = dict(outer_wall_ns=row["wall_ns"])
        if repeated:
            actual = read(path.parent / "stdout.bin")
            assert actual == row["driver_phases"]
            assert set(actual) == {"schema", "repeat", "warmup", "invoke_ns", "loop_ns", "checked_bytes", "fnv1a64"}
            assert (actual["schema"], actual["repeat"], actual["warmup"], actual["checked_bytes"], actual["fnv1a64"]) == ("order-book-repeat-v1", 1000, 0, 2509000, doc["expected_fnv1a64"])
            assert all(type(actual[k]) is int and actual[k] >= 0 for k in ("repeat", "warmup", "invoke_ns", "loop_ns", "checked_bytes"))
            assert actual["loop_ns"] >= actual["invoke_ns"] and not row["stderr"]["bytes"]
            metrics.update(invoke_ns=actual["invoke_ns"], loop_ns=actual["loop_ns"], invoke_ns_per_entry=actual["invoke_ns"] / 1000, loop_ns_per_entry=actual["loop_ns"] / 1000)
        else:
            assert row["stdout"]["sha256"] == doc["expected_stdout_sha256"]
        rows.append(dict(arm=arm, block=row["block"], metrics=metrics, stderr_bytes=row["stderr"]["bytes"]))
        identity.append(pin(path))
    assert len(rows) == cell["samples"] and len({(r["arm"], r["block"]) for r in rows}) == cell["samples"]
    report = dict(cell=cell["cell"], kind=cell["kind"], samples=len(rows), failures=[], censored=0,
        profile="Rust opt2/debug0; memory-sink repeated entry" if repeated else doc["profile"],
        repeat=1000 if repeated else None, warmup=0 if repeated else None,
        sources=[cell, pin(directory / "completion.json")] if not repeated else [cell],
        result_identity_sha256=hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
        result_identity_encoding="compact sorted-key JSON list of path/sha256 pairs in sorted block-directory order",
        stderr_nonempty_rows=sum(r["stderr_bytes"] > 0 for r in rows), arms={})
    lookup = {(r["block"], r["arm"]): r for r in rows}
    for i, arm in enumerate(collect.ARMS):
        selected = sorted((r for r in rows if r["arm"] == arm), key=lambda r: r["block"])
        value = dict(samples=len(selected), metrics={})
        for metric in rows[0]["metrics"]:
            absolute = stats([r["metrics"][metric] for r in selected])
            paired = {}
            for label, base in (("to_B", "B"), ("to_previous", collect.ARMS[i - 1] if i else None)):
                ratios = [dict(block=r["block"] + 1, ratio=r["metrics"][metric] / lookup[(r["block"], base)]["metrics"][metric]) for r in selected] if base else None
                paired[label] = dict(summary=stats([r["ratio"] for r in ratios]), block_ratios=ratios) if ratios else None
            value["metrics"][metric] = dict(absolute=absolute, paired=paired)
        report["arms"][arm] = value
    print(json.dumps(report, separators=(",", ":")))


if __name__ == "__main__":
    main()
