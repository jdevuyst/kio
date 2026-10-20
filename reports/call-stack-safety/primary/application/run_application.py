"""Fixed six process cells and one repeated application cell; existing workers."""
import argparse
import datetime
import importlib.util
import json
import os
import pathlib
import resource
import shutil
import signal
import subprocess
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
PRIMARY = HERE.parent
sys.path.insert(0, str(PRIMARY))
import collect
from prepare import file_inventory, schedule


def identities(rows):
    for row in rows:
        if collect.digest(row["path"]) != row["sha256"]:
            raise ValueError("changed identity: " + row["path"])


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def main():
    begun = time.monotonic()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True, type=pathlib.Path)
    parser.add_argument("--reviewed-manifest-sha256", required=True)
    parser.add_argument("--reviewed-script-sha256", required=True)
    parser.add_argument("--review", type=pathlib.Path, required=True)
    parser.add_argument("--reviewed-review-sha256", required=True)
    args = parser.parse_args()
    manifest_path = HERE / "application-timing/manifest.json"
    identities([{"path": str(manifest_path), "sha256": args.reviewed_manifest_sha256},
                {"path": __file__, "sha256": args.reviewed_script_sha256},
                {"path": str(args.review), "sha256": args.reviewed_review_sha256}])
    manifest = json.loads(manifest_path.read_text())
    cells = manifest["cells"]
    if len(cells) != 7 or manifest["total_samples"] != 275 or manifest["dispatch_seconds"] != 1200:
        raise ValueError("unexpected authorized scope")
    if os.environ.get("TMPDIR") != str(collect.TEMP_ROOT) or os.environ.get("GIT_NO_REPLACE_OBJECTS") != "1" or resource.getrlimit(resource.RLIMIT_CORE)[0] != 0:
        raise ValueError("temporary root/Git isolation/default core policy mismatch")
    if os.environ.get("PYTHONDONTWRITEBYTECODE") != "1":
        raise ValueError("metadata bytecode writes must stay disabled")
    identities(manifest["sources"] + [manifest["protocol"], manifest["python_runtime_receipt"]] + cells)
    runtime = json.loads(pathlib.Path(manifest["python_runtime_receipt"]["path"]).read_text())
    for tree in runtime["python_trees"]:
        root = pathlib.Path(tree["root"])
        names = sorted(p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file())
        if file_inventory(root, names) != {k: v for k, v in tree.items() if k != "root"}:
            raise ValueError("Python membership/identity changed; no retry")
    if any(pathlib.Path(p).exists() for p in runtime["absent_configuration_at_preparation"]):
        raise ValueError("Python startup configuration changed")
    output = HERE / "application-timing/results-01"
    output.mkdir(exist_ok=False)
    collect.save(output / "launch.json", {"utc": utc(), "manifest_sha256": args.reviewed_manifest_sha256,
        "script_sha256": args.reviewed_script_sha256, "review": str(args.review), "review_sha256": args.reviewed_review_sha256,
        "owner_pid": os.getpid(), "stack_limit": list(resource.getrlimit(resource.RLIMIT_STACK)),
        "core_limit": list(resource.getrlimit(resource.RLIMIT_CORE)),
        "deadline_policy": "1200s outer window; active process collector TERM on expiry, up to65s drain. Repeated probes not admitted after1135s."})
    interrupted = False
    def signal_handler(signum, frame):
        nonlocal interrupted
        interrupted = True
    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, signal_handler)
    completed = []
    state = "complete"
    child = None
    def drain():
        if child is not None and child.poll() is None:
            child.terminate()
            limit = time.monotonic() + 65
            while child.poll() is None and time.monotonic() < limit:
                try:
                    child.wait(timeout=min(1, limit - time.monotonic()))
                except subprocess.TimeoutExpired:
                    pass
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=5)
                raise ValueError("collector exceeded normal owned drain; inspect descendants before any further workload")
    try:
        for cell in cells:
            if interrupted or time.monotonic() - begun >= 1200:
                state = "stopped-outer-window-or-interruption"
                break
            if shutil.disk_usage(HERE).free < 10 * 1024**3:
                raise ValueError("below10GiB floor")
            identities([cell] + manifest["sources"])
            if interrupted or time.monotonic() - begun >= 1200:
                state = "stopped-outer-window-or-interruption"
                break
            name = cell["cell"]
            if cell["kind"] == "repeated-entry":
                doc = json.loads(pathlib.Path(cell["path"]).read_text())
                if (doc["repeat"], doc["warmup"], doc["blocks"], doc["schedule_seed"], doc["expected_checked_bytes"]) != (1000,0,10,20261015,2509000):
                    raise ValueError("repeated-entry input scope changed")
                identities(doc["sources"] + [doc["oracle"]])
                checksum = 0xcbf29ce484222325
                oracle = pathlib.Path(doc["oracle"]["path"]).read_bytes()
                if len(oracle) != 2509:
                    raise ValueError("application oracle length changed")
                for _ in range(1000):
                    for byte in oracle:
                        checksum = ((checksum ^ byte) * 0x100000001b3) & ((1 << 64)-1)
                if doc["expected_fnv1a64"] != f"{checksum:016x}":
                    raise ValueError("independent repeated checksum mismatch")
                helper = PRIMARY / "python-smoke/run.py"
                if collect.digest(helper) != "eb4dfb70693295c336233bbc614677c362bbb55fec37842bf23812407e07cee0":
                    raise ValueError("owned invoke helper changed")
                spec = importlib.util.spec_from_file_location("existing_smoke", helper)
                base = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(base)
                folder = output / name
                folder.mkdir()
                record = {"cell": name, "start_utc": utc(), "status": "running", "completed": []}
                completed.append(record)
                for block, order in enumerate(schedule(20261015)):
                    for position, label in enumerate(order):
                        arm = next(a for a in doc["arms"] if a["id"] == label)
                        identities(arm["files"] + [doc["stdin"], cell])
                        if interrupted or time.monotonic() - begun >= 1135:
                            state = "stopped-outer-window-or-interruption"
                            break
                        if arm["argv"][1:] != ["1000", "0"]:
                            raise ValueError("repeated argv differs from common input")
                        slot = folder / f"block-{block:02d}-{position}-{label}"
                        process, ok = base.invoke(arm["argv"], pathlib.Path(arm["cwd"]), doc["env"], pathlib.Path(doc["stdin"]["path"]), slot, 60, 1048576)
                        errors = []
                        actual = json.loads((slot / "stdout.bin").read_text()) if ok else None
                        if not ok:
                            errors.append("runtime failure/censor/interruption")
                        elif not isinstance(actual, dict) or set(actual) != {"schema","repeat","warmup","invoke_ns","loop_ns","checked_bytes","fnv1a64"}:
                            errors.append("invalid repeated result fields")
                        elif (actual["schema"], actual["repeat"], actual["warmup"], actual["checked_bytes"], actual["fnv1a64"]) != ("order-book-repeat-v1",1000,0,2509000,doc["expected_fnv1a64"]):
                            errors.append("application input/output/checksum mismatch")
                        elif any(type(actual[k]) is not int or actual[k] < 0 for k in ("repeat","warmup","invoke_ns","loop_ns","checked_bytes")) or actual["loop_ns"] < actual["invoke_ns"]:
                            errors.append("invalid timing/type fields")
                        if process["stderr"]["bytes"]:
                            errors.append("nonempty stderr")
                        identities(arm["files"] + [doc["stdin"], cell])
                        status = "success" if not errors else "stopped-invalid-sample"
                        collect.save(slot / "result.json", {**process, "status": status, "cell": name, "block": block, "position": position,
                            "arm": arm, "trusted_command_sha256": cell["sha256"], "driver_phases": actual, "oracle_errors": errors})
                        record["completed"].append({"block": block,"position":position,"arm":label,"status":status})
                        if errors:
                            raise ValueError("preserved repeated sample failure: " + str(slot))
                    if state != "complete":
                        break
                record.update(end_utc=utc(),status="complete" if len(record["completed"])==50 and state=="complete" else "stopped-cell")
                collect.save(folder / "completion.json", {"state":record["status"],"completed":record["completed"]})
                print(name + " " + record["status"], flush=True)
                if state != "complete":
                    break
                continue
            argv = [sys.executable, "-B", str(PRIMARY / "collect.py"), "run-reviewed", cell["path"],
                    "--reviewed-sha256", cell["sha256"], "--output", str(output / name), "--repo", str(args.repo)]
            record = {"cell": name, "start_utc": utc(), "argv": argv, "status": "dispatch-attempted"}
            completed.append(record)
            with (output / (name + ".launch.log")).open("xb") as log:
                child = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                record["collector_pid"] = child.pid
                while child.poll() is None:
                    if interrupted or time.monotonic() - begun >= 1200:
                        state = "stopped-outer-window-or-interruption"
                        drain()
                        break
                    try:
                        child.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        pass
            record.update(end_utc=utc(), outer_exit=child.returncode)
            completion_path = output / name / "completion.json"
            record["collector_completion"] = json.loads(completion_path.read_text()) if completion_path.exists() else None
            record["status"] = "complete" if child.returncode == 0 and record["collector_completion"] and record["collector_completion"]["state"] == "complete" else "stopped-cell"
            print(name + " " + record["status"], flush=True)
            if state != "complete" or record["status"] != "complete":
                if state == "complete":
                    state = "stopped-cell-failure"
                break
    except BaseException as error:
        state = "stopped-controller-or-resource-error"
        collect.save(output / "error.json", {"error": str(error)})
        raise
    finally:
        drain_error = None
        try:
            drain()
        except BaseException as error:
            state = "stopped-drain-failure"
            drain_error = str(error)
        collect.save(output / "completion.json", {"state": state, "cells": completed, "end_utc": utc(),
            "elapsed_seconds": time.monotonic() - begun, "unattempted_cells": [c["cell"] for c in cells[len(completed):]],
            "free_bytes": shutil.disk_usage(HERE).free, "drain_error": drain_error})
    return 0 if state == "complete" else 2


if __name__ == "__main__":
    sys.exit(main())
