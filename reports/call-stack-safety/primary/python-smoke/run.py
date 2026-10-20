"""One fixed five-arm Python correctness smoke; never a performance sampler."""

import argparse
import json
import os
import pathlib
import shutil
import sys

ROOT = pathlib.Path(__file__).resolve().parent
PRIMARY = ROOT.parent
sys.path.insert(0, str(PRIMARY))
import collect
import oracle
from prepare import file_inventory

MODES = ("direct", "native", "tail", "pending", "callback-tail", "callback-pending")
COPIES = ("workload/bench.kio", "workload/bench.pkg.kio", "drivers/python.py")
ENV_KEYS = collect.ENV_KEYS | {"CARGO_HOME", "KIO_TOOLCHAINS_ROOT", "RUSTC_WRAPPER",
                             "KIO_TEST_RUNNER_COMPILER_WRAPPER", "SCCACHE_IDLE_TIMEOUT"}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def source_check(plan):
    require(plan["schema"] == "kio-python-smoke-v1", "wrong smoke manifest schema")
    require(collect.digest(PRIMARY.parent / "prepare.py") == plan["prepare_sha256"], "inventory helper changed")
    actual = file_inventory(PRIMARY, [row["path"] for row in plan["sources"]["files"]])
    require(actual == plan["sources"], "prepared source/helper bytes or modes changed")
    pins = json.loads((PRIMARY / "inventory.json").read_text())["arms"]
    require([(a["id"], a["commit"]) for a in plan["arms"]] == [(a["id"], a["commit"]) for a in pins],
            "smoke arms differ from current strict pins")
    require([a["id"] for a in plan["arms"]] == ["B", "T", "C", "S", "P"], "wrong arm order")
    for arm in plan["arms"]:
        for name in COPIES:
            require((ROOT / "arms" / arm["id"] / name).read_bytes() == (PRIMARY / name).read_bytes(),
                    "arm-owned source differs from unchanged common source")
    for config in plan["inputs"]:
        require(oracle.config_errors({**config, "arm": "B"}) == [], "invalid witness input")
    require(plan["inputs"] == [dict(mode=mode, depth=0 if mode in ("direct", "native") else 3,
                                   repeat=1, seed=7, warmup=0, purpose="witness") for mode in MODES],
            "the fixed six-witness scope changed")


def authenticate_compilers(plan):
    build = pathlib.Path(plan["compiler_build"])
    require(collect.digest(build / "RECEIPT.md") == plan["compiler_receipt_sha256"], "compiler receipt changed")
    require((build / "exit").read_text().strip() == "0" and
            (build / "active-arm").read_text().strip() == "all-five-complete", "compiler owner has not completed")
    identities = {}
    for arm in plan["arms"]:
        folder = build / arm["id"]
        require((folder / "exit").read_text().strip() == "0", "compiler build failed: " + arm["id"])
        require((folder / "source-identity").read_text().splitlines() == [arm["commit"], arm["tree"]],
                "compiler source identity mismatch: " + arm["id"])
        binary = folder / "kio"
        require(not binary.is_symlink() and binary.is_file() and os.access(binary, os.X_OK), "missing immutable compiler")
        expected = (folder / "binary.sha256").read_text().split()
        require(expected == [arm["compiler_sha256"], str(binary)] and collect.digest(binary) == arm["compiler_sha256"],
                "compiler binary hash mismatch: " + arm["id"])
        receipts = ("source-identity", "source-inputs.sha256", "source-check-after.log", "command",
                    "exit", "binary.sha256", "version.txt", "cargo.jsonl", "build.log")
        identities[arm["id"]] = {"compiler": str(binary), "sha256": expected[0],
                                 "receipts": file_inventory(folder, list(receipts))}
    return identities


def invoke(argv, cwd, env, stdin, folder, timeout, cap=1048576):
    require(shutil.disk_usage(collect.TEMP_ROOT).free >= 10 * 1024**3, "below 10 GiB disk floor")
    folder.mkdir()
    collect.save(folder / "request.json", {"command": {"argv": argv, "cwd": str(cwd)},
                 "env": env, "stdin": str(stdin), "timeout": timeout, "cap": cap})
    code, interrupted = collect.owned_worker(folder)
    collect.save(folder / "ownership.json", {"worker_returncode": code, "interrupted": interrupted})
    require(not interrupted, "smoke interrupted; no further command is authorized by this run")
    require(code == 0 and (folder / "process.json").is_file(), "process supervisor failed")
    row = json.loads((folder / "process.json").read_text())
    return row, row["status"] == "exited" and row["returncode"] == 0


def run(plan, output):
    require(output.is_absolute() and output.parent == ROOT and not output.exists(),
            "output must be a new direct child of this smoke packet")
    output.mkdir()
    summary = {"scope": "correctness/firing only; recorded durations are not performance data", "arms": [], "state": "running"}
    try:
        source_check(plan)
        require(os.environ.get("TMPDIR") == str(collect.TEMP_ROOT) and os.environ.get("GIT_NO_REPLACE_OBJECTS") == "1",
                "home TMPDIR and disabled Git replacements are required")
        env = {key: os.environ[key] for key in sorted(ENV_KEYS) if key in os.environ}
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        compilers = authenticate_compilers(plan)
        require(collect.digest(plan["scheduler"]) == plan["scheduler_sha256"], "scheduler source changed")
        require(collect.digest(plan["scheduler_binary"]) == plan["scheduler_binary_sha256"], "scheduler binary changed")
        env["KIO_CI_SCHEDULER_BIN"] = plan["scheduler_binary"]
        shell = shutil.which("sh")
        require(shell is not None, "sh is unavailable in the pinned tool environment")
        collect.save(output / "provenance.json", {"manifest": plan, "compilers": compilers, "env_allowlist": env,
                     "python": {"executable": sys.executable, "sha256": collect.digest(sys.executable), "version": sys.version},
                     "shell": {"executable": shell, "sha256": collect.digest(shell)}})
        stdin = output / "empty-stdin"
        stdin.touch()
        for arm in plan["arms"]:
            source_check(plan)
            label = arm["id"]
            owned = output / label
            owned.mkdir()
            for name in COPIES:
                destination = owned / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / "arms" / label / name, destination)
            collect.save(owned / "source-inventory.json", file_inventory(owned, list(COPIES)))
            identity = compilers[label]
            require(collect.digest(identity["compiler"]) == identity["sha256"], "compiler changed before emission")
            argv = [shell, plan["scheduler"], "--resource", "compiler", "--", identity["compiler"], "build", "python"]
            # Emission writes artifacts: runtime's 1 MiB file cap is inappropriate here.
            built, ok = invoke(argv, owned / "workload", env, stdin, owned / "emit", 180, 64 * 1024**2)
            result = {"arm": label, "build_ok": ok, "witnesses": []}
            summary["arms"].append(result)
            if not ok:
                result["skipped_modes"] = list(MODES)
                print(label + " emission failed; retained logs and skipped witnesses", flush=True)
                raise ValueError(label + " emission failed; stop for source/API diagnosis")
            emitted = owned / "workload/out/python"
            names = [p.relative_to(emitted).as_posix() for p in emitted.rglob("*") if p.is_file()]
            artifacts = file_inventory(emitted, names)
            collect.save(owned / "emitted-source-inventory.json", artifacts)
            require("bench.py" in names, "successful Python build omitted its public bench.py artifact")
            for config in plan["inputs"]:
                source_check(plan)
                require(file_inventory(emitted, names) == artifacts, "emitted artifact changed before witness")
                actual = {**config, "arm": label}
                folder = owned / config["mode"]
                argv = [sys.executable, "-B", str(owned / "drivers/python.py"), str(emitted / "bench.py"), label,
                        config["mode"], str(config["depth"]), "1", "7", "0", "witness"]
                row, passed = invoke(argv, owned, env, stdin, folder, 60)
                errors = []
                if passed:
                    try:
                        driver = json.loads(pathlib.Path(row["stdout"]["path"]).read_text())
                        errors = oracle.result_errors(actual, driver)
                        if driver.get("backend") != "python":
                            errors.append("witness is not the Python backend")
                    except (ValueError, TypeError, AttributeError) as error:
                        errors.append(str(error))
                    passed = not errors
                check = {"input": actual, "oracle": oracle.oracle(actual), "passed": passed, "errors": errors}
                collect.save(folder / "oracle-check.json", check)
                result["witnesses"].append(check)
                require(file_inventory(emitted, names) == artifacts, "emitted artifact changed during witness")
                print(label + " " + config["mode"] + (" PASS" if passed else " FAIL"), flush=True)
                require(passed, label + " " + config["mode"] + " failed; stop for source/API diagnosis")
        source_check(plan)
        require(all(collect.digest(item["compiler"]) == item["sha256"] for item in compilers.values()),
                "compiler changed during smoke")
        passed = len(summary["arms"]) == 5 and all(a["build_ok"] and len(a["witnesses"]) == 6 and
                 all(w["passed"] for w in a["witnesses"]) for a in summary["arms"])
        summary["state"] = "passed" if passed else "failed"
    except Exception as error:
        summary.update(state="stopped", error=str(error))
    finally:
        collect.save(output / "summary.json", summary)
    return int(summary["state"] != "passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--selfcheck", action="store_true")
    parser.add_argument("--cleanup-complete", action="store_true")
    parser.add_argument("--manifest-sha256")
    parser.add_argument("--output", type=pathlib.Path)
    args = parser.parse_args()
    path = ROOT / "manifest.json"
    plan = json.loads(path.read_text())
    if args.selfcheck:
        source_check(plan)
        print(json.dumps({"source_checks": "passed", "cells": 30, "compiler_or_host_runs": 0}))
        return 0
    require(args.cleanup_complete and args.output is not None, "root cleanup-complete release and new output path required")
    require(collect.digest(path) == args.manifest_sha256, "pass the exact independently reviewed manifest SHA-256")
    return run(plan, args.output)


if __name__ == "__main__":
    sys.exit(main())
