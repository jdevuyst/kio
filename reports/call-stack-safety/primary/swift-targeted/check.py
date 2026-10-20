"""Offline Swift-panel preparation checks; never launches a compiler or driver."""

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

MODES = ("direct", "native", "host-loop", "callback-tail", "callback-pending", "callback-extract")
FILES = ("check.py", "build-receipt.template.json",
         "workload/bench_a.pkg.kio", "workload/bench.kio",
         "workload/peer/bench_b.pkg.kio", "workload/peer/bench.kio",
         "host/main.swift", "host/baseline.swift", "host/staged.swift")


def core_config(config):
    return {**config, "mode": "callback-tail" if config.get("mode") == "callback-extract" else config.get("mode")}


def expected_owners(config):
    labels = primary.oracle(core_config(config))["trace"]
    return [[0 if label == 0 else (label - 1) % 2, label] for label in labels]


def result_errors(config, row):
    if not isinstance(config, dict) or not isinstance(row, dict):
        return ["configuration/result must be objects"]
    errors = []
    if config.get("mode") not in MODES:
        errors.append("unknown Swift panel mode")
    projected = core_config(config)
    config_errors = primary.config_errors(projected)
    if config_errors:
        return errors + config_errors
    if row.get("schema") != "kio-swift-targeted-driver-v1" or row.get("backend") != "swift":
        errors.append("Swift result identity mismatch")
    if row.get("topology") != "alternating-independent-modules" or row.get("mode") != config["mode"]:
        errors.append("Swift topology/mode mismatch")
    errors += primary.result_errors(projected, {**row, "schema": "kio-primary-driver-v1",
                                               "backend": "python", "mode": projected["mode"]})
    owners = row.get("owner_trace")
    if not isinstance(owners, list) or any(not isinstance(pair, list) or len(pair) != 2 or any(type(value) is not int for value in pair) for pair in owners):
        errors.append("invalid package-owner trace shape")
    elif owners != expected_owners(config):
        errors.append("independent package-owner trace mismatch")
    count = config["depth"] + 1 if config["mode"].startswith("callback-") else 0
    for key, expected in (("root_slots_before_release", count), ("released_root_slots", count), ("root_slots_after_release", 0)):
        if type(row.get(key)) is not int or row.get(key) != expected:
            errors.append("root-slot observation mismatch: " + key)
    if "capture_destruction" not in row or row["capture_destruction"] is not None or not isinstance(row.get("capture_destruction_reason"), str) or not row["capture_destruction_reason"]:
        errors.append("unavailable capture-destruction evidence must be explicit null with reason")
    return errors


def selfcheck():
    if not __debug__:
        raise ValueError("selfcheck requires assertions")
    ast.parse(pathlib.Path(__file__).read_text())
    common = (ROOT.parent / "workload/bench.kio").read_bytes()
    assert (ROOT / "workload/bench.kio").read_bytes() == common
    assert (ROOT / "workload/peer/bench.kio").read_bytes() == common
    receipt = json.loads((ROOT / "build-receipt.template.json").read_text())
    assert receipt["state"] == "incomplete-source-only"
    assert receipt["profile_flags"] == {"development": ["-Onone"], "optimized": ["-O"]}
    assert receipt["support_required_by_arm"] == {"B": False, "T": False, "C": True, "S": True, "P": True}
    assert all(step["argv"] is None and step["process_receipt_sha256"] is None for step in receipt["native_steps"].values())
    for arm in ("B", "T", "C", "S", "P"):
        for mode in MODES:
            for purpose in ("witness", "time"):
                config = {"arm": arm, "mode": mode, "depth": 0 if mode in ("direct", "native") else 3,
                          "repeat": 1 if purpose == "witness" else 2, "seed": 7, "warmup": 0, "purpose": purpose}
                expected = primary.oracle(core_config(config))
                roots = config["depth"] + 1 if mode.startswith("callback-") else 0
                row = {**config, "schema": "kio-swift-targeted-driver-v1", "backend": "swift",
                       "topology": "alternating-independent-modules", **expected, "expected": expected["checksum"],
                       "owner_trace": expected_owners(config), **{key: 1 for key in primary.PHASES}, "total_phases_ns": 5,
                       "root_slots_before_release": roots, "released_root_slots": roots, "root_slots_after_release": 0,
                       "capture_destruction": None, "capture_destruction_reason": "not instrumented"}
                assert result_errors(config, row) == []
                assert result_errors(config, {**row, "checksum": row["checksum"] + 1})
                assert result_errors(config, {**row, "owner_trace": [[999, 0]]})
                assert result_errors(config, {**row, "root_slots_after_release": 1})
                assert result_errors(config, {**row, "capture_destruction": True})
                assert result_errors(config, {**row, "backend": "go"})
    print(json.dumps({"status": "Swift-panel offline source/oracle selfchecks passed", "compiler_or_driver_runs": 0}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("selfcheck")
    commands.add_parser("inventory")
    check = commands.add_parser("check-result")
    check.add_argument("config", type=pathlib.Path)
    check.add_argument("result", type=pathlib.Path)
    args = parser.parse_args()
    if args.command == "selfcheck":
        selfcheck()
    elif args.command == "inventory":
        print(json.dumps(file_inventory(ROOT, FILES), indent=2, sort_keys=True))
    else:
        errors = result_errors(json.loads(args.config.read_text()), json.loads(args.result.read_text()))
        print(json.dumps({"valid": not errors, "errors": errors}))
        return bool(errors)
    return 0


if __name__ == "__main__":
    sys.exit(main())
