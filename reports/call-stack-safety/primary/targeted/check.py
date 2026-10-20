"""Offline targeted source/oracle checks. Never imports or executes a driver."""

import argparse
import ast
import importlib.util
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("primary_oracle", ROOT.parent / "oracle.py")
primary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(primary)
from prepare import file_inventory  # noqa: E402

FILES = ("check.py", "go.mod.in", "workload/bench.kio", "workload/bench.pkg.kio",
         "go/main.go", "go/baseline.go", "go/staged.go", "java/Main.java",
         "ts/surface.ts", "ts/baseline.ts", "ts/staged.ts")


def result_errors(backend, config, row):
    if backend not in ("go", "java") or not isinstance(config, dict) or not isinstance(row, dict):
        return ["expected a known targeted backend and object config/result"]
    errors = []
    if row.get("schema") != "kio-targeted-driver-v1" or row.get("backend") != backend:
        errors.append("targeted result identity mismatch")
    if row.get("mode") != config.get("mode"):
        errors.append("targeted mode mismatch")
    mode = config.get("mode")
    if mode == "callback-extract":
        if backend != "go":
            errors.append("native extraction control exists only in the Go draft")
        mode = "callback-tail"
    # Reuse the independently checked arithmetic/effect/phase rules. The schema
    # and target identity are checked above, not accepted from this projection.
    errors += primary.result_errors({**config, "mode": mode},
                                   {**row, "schema": "kio-primary-driver-v1", "backend": "python", "mode": mode})
    return errors


def compare_js(js, ts_js):
    inventories = [file_inventory(path.parent, [path.name]) for path in (js, ts_js)]
    first, second = [entry["files"][0] for entry in inventories]
    identical = first["sha256"] == second["sha256"] and first["bytes"] == second["bytes"]
    return {"identical": identical, "js": first, "ts_js": second,
            "scope": "exact runtime bytes only; no declaration or runtime validation"}


def selfcheck():
    if not __debug__:
        raise ValueError("selfcheck requires assertions")
    ast.parse(pathlib.Path(__file__).read_text())
    assert (ROOT / "workload/bench.kio").read_bytes() == (ROOT.parent / "workload/bench.kio").read_bytes()
    for backend in ("go", "java"):
        for mode in (*sorted(primary.MODES), "callback-extract"):
            config = {"arm": "B", "mode": mode, "depth": 0 if mode in ("direct", "native") else 3,
                      "repeat": 1, "seed": 7, "warmup": 0, "purpose": "witness"}
            oracle_config = {**config, "mode": "callback-tail" if mode == "callback-extract" else mode}
            expected = primary.oracle(oracle_config)
            row = {**config, "schema": "kio-targeted-driver-v1", "backend": backend,
                   **expected, "expected": expected["checksum"],
                   **{key: 1 for key in primary.PHASES}, "total_phases_ns": 5}
            if backend == "java" and mode == "callback-extract":
                assert result_errors(backend, config, row)
                continue
            assert result_errors(backend, config, row) == []
            assert result_errors(backend, config, {**row, "checksum": row["checksum"] + 1})
            assert result_errors(backend, config, {**row, "trace": [999]})
            assert result_errors(backend, config, {**row, "backend": "python"})
            assert result_errors(backend, config, {**row, "mode": "unknown"})
    assert compare_js(ROOT / "workload/bench.kio", ROOT.parent / "workload/bench.kio")["identical"]
    assert not compare_js(ROOT / "workload/bench.kio", ROOT / "workload/bench.pkg.kio")["identical"]
    print(json.dumps({"status": "targeted offline source/oracle selfchecks passed", "executed_drivers": 0}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("selfcheck")
    commands.add_parser("inventory")
    check = commands.add_parser("check-result")
    check.add_argument("backend", choices=("go", "java"))
    check.add_argument("config", type=pathlib.Path)
    check.add_argument("result", type=pathlib.Path)
    compare = commands.add_parser("compare-js")
    compare.add_argument("js", type=pathlib.Path)
    compare.add_argument("ts_js", type=pathlib.Path)
    args = parser.parse_args()
    if args.command == "selfcheck":
        selfcheck()
    elif args.command == "inventory":
        print(json.dumps(file_inventory(ROOT, FILES), indent=2, sort_keys=True))
    elif args.command == "compare-js":
        result = compare_js(args.js, args.ts_js)
        print(json.dumps(result, indent=2, sort_keys=True))
        return not result["identical"]
    else:
        errors = result_errors(args.backend, json.loads(args.config.read_text()), json.loads(args.result.read_text()))
        print(json.dumps({"valid": not errors, "errors": errors}))
        return bool(errors)
    return 0


if __name__ == "__main__":
    sys.exit(main())
