"""Descriptive arithmetic over retained results only; no workload execution."""
import hashlib
import json
import pathlib
import statistics
import sys

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import collect

ROOT = HERE / "finalized-v1/results-01"
MANIFEST = HERE / "finalized-v1/manifest.json"
MAPPING = HERE / "continuation-map.json"
PHASES = ("package_ns", "construct_ns", "warmup_ns", "invoke_ns", "release_roots_ns", "total_phases_ns")


def read(path):
    return json.loads(pathlib.Path(path).read_text())


def pin(path):
    return dict(path=str(path), sha256=collect.digest(path))


def digest_rows(rows):
    return hashlib.sha256(json.dumps(rows, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def stats(values):
    return dict(n=len(values), median=statistics.median(values), min=min(values), max=max(values))


def load(row, directory, offset, window):
    path = pathlib.Path(row["path"])
    assert collect.digest(path) == row["sha256"]
    doc = read(path)
    completion = read(directory / "completion.json")
    assert completion["state"] == "complete" and not completion["failed_arms"]
    assert len(completion["completed"]) == doc["blocks"] * 5
    record = read(directory / "cell.json")
    assert record["trusted_command_sha256"] == row["sha256"] and record["commands"] == doc
    rows = []
    for result in sorted(directory.glob("block-*/result.json")):
        value = read(result)
        driver = read(result.parent / "stdout.bin")
        assert value["status"] == "success" and value["returncode"] == 0
        assert not value["censored"] and not value["oracle_errors"]
        assert value["stderr"]["bytes"] == 0 and value["stack_limit"] == [8388608, -1]
        assert value["timeout_seconds"] == 60 and value["max_output_bytes"] == 1048576
        assert value["trusted_command_sha256"] == row["sha256"]
        for stream in ("stdout", "stderr"):
            stream_path = result.parent / (stream + ".bin")
            assert collect.digest(stream_path) == value[stream]["sha256"]
            assert stream_path.stat().st_size == value[stream]["bytes"]
        assert driver == value["driver_phases"]
        assert not collect.driver_errors(doc, value["arm"], driver)
        arm = value["arm"]["id"]
        assert record["blocks"][value["block"]][value["position"]] == arm
        rows.append(dict(arm=arm, block=value["block"] + offset, window=window,
            repeat=driver["repeat"], phases={key: driver[key] for key in PHASES},
            outer_wall_ns=value["wall_ns"], rss_bytes=value["max_child_rss_bytes"], result=pin(result)))
    assert len(rows) == doc["blocks"] * 5
    assert len({(r["arm"], r["block"]) for r in rows}) == len(rows)
    return rows, dict(command=pin(path), cell=pin(directory / "cell.json"), completion=pin(directory / "completion.json"))


def summarize(name, doc, rows, sources, combined):
    lookup = {(r["block"], r["arm"]): r for r in rows}
    assert len(lookup) == len(rows)
    repeats = {r["repeat"] for r in rows}
    assert repeats == {doc["input"]["repeat"]}
    result = dict(cell=name, backend=doc["backend"], input=doc["input"], profile=doc["profile"],
        samples=len(rows), failures=0, censored=0, combined_original=combined, sources=sources,
        scope="host-driven IO-bind control, not Kio do/bind/guest pending" if doc["input"]["mode"].startswith("io-bind") else "public callable scalar workload",
        result_identities_sha256=digest_rows([r["result"] for r in rows]),
        result_identity_encoding="sha256 of compact sorted-key JSON list of path/sha256 pairs in sorted block-directory order, original window then continuation when combined", arms={})
    for i, arm in enumerate(collect.ARMS):
        selected = sorted((r for r in rows if r["arm"] == arm), key=lambda r: r["block"])
        previous = collect.ARMS[i - 1] if i else None
        value = dict(samples=len(selected), blocks=[r["block"] for r in selected],
            phases_ns={key: stats([r["phases"][key] for r in selected]) for key in PHASES},
            invoke_ns_per_repeat=stats([r["phases"]["invoke_ns"] / r["repeat"] for r in selected]),
            outer_wall_ns=stats([r["outer_wall_ns"] for r in selected]),
            coarse_child_peak_rss_bytes=stats([r["rss_bytes"] for r in selected]), paired={})
        for key in PHASES:
            ratios = {}
            for label, base in (("to_B", "B"), ("to_previous", previous)):
                if base is None:
                    ratios[label] = None
                    continue
                observed = [(r["block"], r["phases"][key], lookup[(r["block"], base)]["phases"][key]) for r in selected]
                valid = [dict(block=b, ratio=n / d) for b, n, d in observed if d != 0]
                ratios[label] = dict(summary=stats([r["ratio"] for r in valid]) if valid else None,
                    matched_block_ratios=valid if key == "invoke_ns" else None,
                    zero_denominator_blocks=[b for b, n, d in observed if d == 0])
            value["paired"][key] = ratios
        value["windows"] = {window: dict(samples=len(group),
            phases_ns={key: stats([r["phases"][key] for r in group]) for key in PHASES},
            outer_wall_ns=stats([r["outer_wall_ns"] for r in group]))
            for window in sorted({r["window"] for r in selected})
            if (group := [r for r in selected if r["window"] == window])}
        result["arms"][arm] = value
    return result


def main():
    manifest = read(MANIFEST)
    completion = read(ROOT / "completion.json")
    assert completion["state"] == "complete" and not completion["unattempted_cells"] and not completion["drain_error"]
    mapping = read(MAPPING)
    names = [row["cell"] for row in manifest["cells"]] + ["combined-" + row["cell"] for row in mapping["cells"]]
    selected = sys.argv[1]
    if selected == "meta":
        print(json.dumps(dict(kind="descriptive-runtime-panel-no-confidence-or-adoption-claim", cells=names,
            sources=[pin(path) for path in (MANIFEST, MAPPING, ROOT / "completion.json", HERE / "FINAL-PROTOCOL.md", pathlib.Path(__file__), HERE.parent / "collect.py", HERE.parent / "runtime-pilot/analyze_pilot.py")],
            new_samples=1225, old_samples=150, unique_samples=1375, derived_sample_memberships=1525,
            note="34 new cell summaries include150 continuations; six combined views reuse those150 plus150 original rows, not independent extra observations.",
            metric="driver phases in nanoseconds, invocation per identical repeat, ratios within matched block; >1 means longer elapsed phase",
            phase_labels=dict(total_phases_ns="sum of the five timed intervals: package, construction, warmup, invocation and root release; excludes gaps and other work; not separately measured end-to-end", outer_wall_ns="separately measured outer process elapsed wall time"),
            boundary_legend=dict(native_0="guest callNative entry invokes a host-native callback; not a host-only control", callback_extract="host-native extraction of a guest-returned callable; not an all-native workload"),
            uncertainty="Observed min/median/max and matched-block ratio ranges only; no confidence interval, independence or adoption inference.",
            limits=["Scalar public-callable probes only; no whole-program generalization.", "Root release is reverse-order root clearing, not proof of capture destruction or collection.", "Peak child RSS is a coarse process high-water mark, not allocated/live/retained heap.", "Java fixed10000-call warmup is descriptive; no stable-JIT claim or calibrated linear-scaling assumption.", "Rust shallow cells are short at unchanged1000000-repeat cap.", "Original versus continuation quiet windows differ, including four added Python stdlib pyc files. Outer-wall aggregates are descriptive mixtures; use explicit window strata for startup analysis.", "total_phases_ns is the sum of the five timed intervals, excluding gaps and other work; outer process wall is separately measured."])))
        return
    if selected.startswith("combined-"):
        original = next(row for row in mapping["cells"] if "combined-" + row["cell"] == selected)
        for key in ("original_frozen_command", "original_cell_record", "original_completion"):
            assert collect.digest(original[key]["path"]) == original[key]["sha256"]
        new = next(row for row in manifest["cells"] if row["kind"] == "continuation" and read(row["path"])["continuation"]["original"] == original["original_frozen_command"])
        oldrow = original["original_frozen_command"]
        olddoc = read(oldrow["path"])
        newdoc = read(new["path"])
        assert olddoc["arms"] == newdoc["arms"] and olddoc["input"] == newdoc["input"]
        oldrows, oldsource = load(oldrow, pathlib.Path(original["original_result_directory"]), 1, "original-pilot")
        newrows, newsource = load(new, ROOT / new["cell"], 6, "next-runtime-window")
        assert sorted({row["block"] for row in oldrows + newrows}) == list(range(1, 11))
        value = summarize(selected, olddoc, oldrows + newrows, [oldsource, newsource], True)
    else:
        row = next(row for row in manifest["cells"] if row["cell"] == selected)
        rows, source = load(row, ROOT / row["cell"], 6 if row["kind"] == "continuation" else 1, "next-runtime-window")
        value = summarize(selected, read(row["path"]), rows, [source], False)
    print(json.dumps(value, separators=(",", ":")))


if __name__ == "__main__":
    main()
