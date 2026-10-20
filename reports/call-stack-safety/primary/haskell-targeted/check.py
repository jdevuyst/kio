"""Offline Haskell-panel source/oracle checks; never launches native tools."""

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

MODES = ("direct", "native", "host-loop", "callback-tail", "callback-pending", "io-bind-native", "io-bind-public")
FILES = ("check.py", "build-receipt.template.json", "host/Main.hs",
         "workload/bench.kio", "workload/bench.pkg.kio")
DEMAND = "every-Int32-result-and-strict-Int64-checksum-before-stop"
RTS_POLICY = "unmodified-defaults-required"


def core_config(config):
    return {**config, "mode": "callback-pending" if config.get("mode") in ("io-bind-native", "io-bind-public") else config.get("mode")}


def root_counts(config):
    return {
        "callback_root_slots_before_release": config["depth"] + 1 if config["mode"].startswith("callback-") else 0,
        "callback_root_slots_after_release": 0,
        "input_root_slots_before_release": config["depth"] + 1 if config["mode"].startswith("io-bind-") else 0,
        "input_root_slots_after_release": 0,
    }


def result_errors(config, row):
    if not isinstance(config, dict) or not isinstance(row, dict):
        return ["configuration/result must be objects"]
    errors = []
    if config.get("mode") not in MODES:
        errors.append("unknown Haskell panel mode")
    projected = core_config(config)
    config_errors = primary.config_errors(projected)
    if config_errors:
        return errors + config_errors
    if row.get("schema") != "kio-haskell-targeted-driver-v1" or row.get("backend") != "haskell" or row.get("mode") != config["mode"]:
        errors.append("Haskell result identity/mode mismatch")
    errors += primary.result_errors(projected, {**row, "schema": "kio-primary-driver-v1",
                                               "backend": "python", "mode": projected["mode"]})
    if not isinstance(row.get("trace"), list) or any(type(value) is not int for value in row["trace"]):
        errors.append("trace must contain exact integers")
    for key, expected in root_counts(config).items():
        if type(row.get(key)) is not int or row.get(key) != expected:
            errors.append("root-slot observation mismatch: " + key)
    for key in ("invoker_root_cleared", "package_root_cleared"):
        if row.get(key) is not True:
            errors.append("root was not observed cleared: " + key)
    if row.get("demand_policy") != DEMAND or row.get("rts_stack_policy") != RTS_POLICY:
        errors.append("demand/RTS policy declaration differs")
    if "capture_collection" not in row or row["capture_collection"] is not None or not isinstance(row.get("capture_collection_reason"), str) or not row["capture_collection_reason"]:
        errors.append("unavailable collection evidence must be null with reason")
    return errors


def selfcheck():
    if not __debug__:
        raise ValueError("selfcheck requires assertions")
    ast.parse(pathlib.Path(__file__).read_text())
    assert (ROOT / "workload/bench.kio").read_bytes() == (ROOT.parent / "workload/bench.kio").read_bytes()
    receipt = json.loads((ROOT / "build-receipt.template.json").read_text())
    assert receipt["state"] == "incomplete-source-only"
    assert receipt["profile_flags"] == {"development": ["-O0"], "optimized": ["-O2"]}
    assert receipt["native_compile_and_link"]["argv"] is None
    assert receipt["native_compile_and_link"]["process_receipt_sha256"] is None
    assert receipt["rts_policy"]["allowed_extra_rts_argv"] == []
    assert receipt["rts_policy"]["stack_policy"] == RTS_POLICY
    for arm in ("B", "T", "C", "S", "P"):
        for mode in MODES:
            for purpose in ("witness", "time"):
                config = {"arm": arm, "mode": mode, "depth": 0 if mode in ("direct", "native") else 3,
                          "repeat": 1 if purpose == "witness" else 2, "seed": 7, "warmup": 0, "purpose": purpose}
                expected = primary.oracle(core_config(config))
                row = {**config, "schema": "kio-haskell-targeted-driver-v1", "backend": "haskell",
                       **expected, "expected": expected["checksum"], **{key: 1 for key in primary.PHASES}, "total_phases_ns": 5,
                       **root_counts(config), "invoker_root_cleared": True, "package_root_cleared": True,
                       "demand_policy": DEMAND, "rts_stack_policy": RTS_POLICY,
                       "capture_collection": None, "capture_collection_reason": "not measured"}
                assert result_errors(config, row) == []
                for change in ({"checksum": row["checksum"] + 1}, {"trace": [False]},
                               {"callback_root_slots_after_release": 1}, {"input_root_slots_before_release": -1},
                               {"invoker_root_cleared": False}, {"demand_policy": "WHNF-only"},
                               {"rts_stack_policy": "raised"}, {"capture_collection": True}, {"backend": "swift"}):
                    assert result_errors(config, {**row, **change})
    print(json.dumps({"status": "Haskell-panel offline source/oracle selfchecks passed", "compiler_or_driver_runs": 0}))


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
