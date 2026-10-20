"""Offline primary-panel checks; never imports/runs drivers or build commands."""

import argparse
import ast
import json
import pathlib
import sys

ROOT = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT.parent))
from prepare import file_inventory, git_read  # noqa: E402

MODES = {"direct", "native", "host-loop", "tail", "pending", "callback-tail", "callback-pending"}
PHASES = ("package_ns", "construct_ns", "warmup_ns", "invoke_ns", "release_roots_ns")
FILES = (
    "inventory.json", "oracle.py", "example-input.json",
    "workload/bench.kio", "workload/bench.pkg.kio", "drivers/python.py",
    "drivers/quickjs.js", "drivers/quickjs/Cargo.toml.in", "drivers/quickjs/src/main.rs",
    "drivers/rust/Cargo.toml.in", "drivers/rust/src/main.rs",
)


def config_errors(c):
    if not isinstance(c, dict):
        return ["configuration must be an object"]
    errors = []
    if set(c) != {"arm", "mode", "depth", "repeat", "seed", "warmup", "purpose"}:
        errors.append("configuration keys differ from the declared schema")
    if c.get("arm") not in ("B", "T", "C", "S", "P"):
        errors.append("unknown arm")
    if c.get("mode") not in MODES:
        errors.append("unknown mode")
    if c.get("purpose") not in ("time", "witness"):
        errors.append("unknown purpose")
    for key, low, high in (("depth", 0, 10000), ("repeat", 1, 1000000), ("seed", 0, 65535), ("warmup", 0, 1000000)):
        value = c.get(key)
        if type(value) is not int or not low <= value <= high:
            errors.append("out-of-range integer: " + key)
    if errors:
        return errors
    if c["mode"] in ("direct", "native") and c["depth"] != 0:
        errors.append("direct/native require depth 0")
    if c["purpose"] == "witness" and (c["depth"] > 8 or c["repeat"] != 1 or c["warmup"] != 0):
        errors.append("witness requires depth <=8, repeat 1, warmup 0")
    return errors


def oracle(c):
    errors = config_errors(c)
    if errors:
        raise ValueError("; ".join(errors))
    # The step function adds label+1. These are independent arithmetic/effect
    # oracles; no driver, generated source, or runtime is inspected or executed.
    delta = (c["depth"] + 1) * (c["depth"] + 2) // 2
    checksum = sum((c["seed"] + i * 17) % 65536 for i in range(c["repeat"])) + c["repeat"] * delta
    trace = list(range(c["depth"] + 1)) if c["mode"] in ("pending", "callback-pending") else list(range(c["depth"], -1, -1))
    return {"checksum": checksum, "trace": trace if c["purpose"] == "witness" else []}


def result_errors(c, row):
    errors = config_errors(c)
    if errors:
        return errors
    if not isinstance(row, dict):
        return ["result must be an object"]
    if row.get("schema") != "kio-primary-driver-v1":
        errors.append("incorrect result schema")
    if row.get("backend") not in ("quickjs", "python", "rust"):
        errors.append("incorrect backend")
    for key, value in c.items():
        if type(row.get(key)) is not type(value) or row.get(key) != value:
            errors.append("input/result mismatch: " + key)
    expected = oracle(c)
    for key in ("checksum", "expected"):
        if type(row.get(key)) is not int or row.get(key) != expected["checksum"]:
            errors.append("independent checksum mismatch: " + key)
    if row.get("trace") != expected["trace"]:
        errors.append("independent trace mismatch")
    for key in (*PHASES, "total_phases_ns"):
        value = row.get(key)
        if type(value) not in (int, float) or not 0 <= value < 1e15:
            errors.append("invalid phase duration: " + key)
    if not any(item.startswith("invalid phase duration") for item in errors):
        if row["total_phases_ns"] != sum(row[k] for k in PHASES):
            errors.append("phase total mismatch")
    return errors


def selfcheck():
    if not __debug__:
        raise ValueError("selfcheck requires Python assertions")
    for path in (ROOT / "oracle.py", ROOT / "drivers/python.py"):
        ast.parse(path.read_text(), filename=str(path))
    for path in (ROOT / "inventory.json", ROOT / "example-input.json"):
        json.loads(path.read_text())
    c = {"arm": "B", "mode": "pending", "depth": 3, "repeat": 1, "seed": 7, "warmup": 0, "purpose": "witness"}
    assert oracle(c) == {"checksum": 17, "trace": [0, 1, 2, 3]}
    assert oracle({**c, "mode": "tail"}) == {"checksum": 17, "trace": [3, 2, 1, 0]}
    for mode in MODES:
        for depth in (0, 1, 8):
            if mode in ("direct", "native") and depth:
                continue
            value = {**c, "mode": mode, "depth": depth}
            expected = oracle(value)
            row = {"schema": "kio-primary-driver-v1", "backend": "python", **value,
                   **expected, "expected": expected["checksum"],
                   **{key: 1 for key in PHASES}, "total_phases_ns": 5}
            assert result_errors(value, row) == []
            assert result_errors(value, {**row, "checksum": row["checksum"] + 1})
            assert result_errors(value, {**row, "trace": [999]})
            assert result_errors(value, {**row, "arm": "P"})
            assert result_errors(value, {**row, "invoke_ns": float("nan")})
    for change in ({"depth": True}, {"depth": 10001}, {"repeat": 0}, {"mode": "unknown"}, {"purpose": "witness", "warmup": 1}):
        assert config_errors({**c, **change})
    assert oracle({**c, "purpose": "time", "depth": 0, "seed": 65535, "repeat": 2})["checksum"] == 65553
    print(json.dumps({"status": "offline source/oracle selfchecks passed", "executed_drivers": 0}))


def source_inventory(repo):
    doc = json.loads((ROOT / "inventory.json").read_text())
    arms = []
    for arm in doc["arms"]:
        commit = arm["commit"]
        arms.append({**arm, "tree": git_read(repo, "rev-parse", commit + "^{tree}"),
                     "parents": git_read(repo, "show", "-s", "--format=%P", commit).split()})
    for previous, current in zip(arms, arms[1:]):
        if current["parents"] != [previous["commit"]]:
            raise ValueError("the exact five commits do not form the declared chain")
    subjects = []
    for subject in doc["existing_subjects"]:
        commit, path = subject["source_commit"], subject["path"]
        tree = git_read(repo, "rev-parse", commit + ":" + path)
        if git_read(repo, "cat-file", "-t", tree) != "tree":
            raise ValueError("subject is not a tracked corpus directory: " + path)
        output = git_read(repo, "rev-parse", commit + ":" + path + "/expected.stdout") if subject["oracle"] == "expected.stdout" else None
        subjects.append({**subject, "source_tree": tree, "oracle_blob": output})
    return {"state": "source-only-unbuilt-unmeasured", "arms": arms,
            "primary_files": file_inventory(ROOT, FILES), "existing_subjects": subjects}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("selfcheck")
    inventory = commands.add_parser("inventory")
    inventory.add_argument("repo", type=pathlib.Path)
    expected = commands.add_parser("oracle")
    expected.add_argument("config", type=pathlib.Path)
    check = commands.add_parser("check-result")
    check.add_argument("config", type=pathlib.Path)
    check.add_argument("result", type=pathlib.Path)
    args = parser.parse_args()
    if args.command == "selfcheck":
        selfcheck()
    elif args.command == "inventory":
        print(json.dumps(source_inventory(args.repo), indent=2, sort_keys=True))
    elif args.command == "oracle":
        print(json.dumps(oracle(json.loads(args.config.read_text())), sort_keys=True))
    else:
        errors = result_errors(json.loads(args.config.read_text()), json.loads(args.result.read_text()))
        print(json.dumps({"valid": not errors, "errors": errors}))
        return bool(errors)
    return 0


if __name__ == "__main__":
    sys.exit(main())
