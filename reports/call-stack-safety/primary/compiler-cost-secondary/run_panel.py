"""Execute frozen compiler commands serially through the existing owned collector."""
import argparse
import importlib.util
import json
import os
import pathlib
import resource
import shutil
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
PRIMARY = HERE.parent
sys.path.insert(0, str(PRIMARY))
import collect
from prepare import file_inventory


def require(ok, message):
    if not ok:
        raise ValueError(message)


def names(root):
    paths = list(root.rglob("*"))
    require(not any(p.is_symlink() for p in paths), "unexpected symlink in retained files")
    return sorted(p.relative_to(root).as_posix() for p in paths if p.is_file())


def allocated():
    return sum(p.stat().st_blocks * 512 for p in HERE.rglob("*") if p.is_file())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reviewed-commands-sha256", required=True)
    parser.add_argument("--reviewed-script-sha256", required=True)
    args = parser.parse_args()
    require(collect.digest(HERE / "commands.json") == args.reviewed_commands_sha256 and
            collect.digest(__file__) == args.reviewed_script_sha256, "reviewed launch hashes changed")
    plan = json.loads((HERE / "commands.json").read_text())
    require(plan["root"] == str(HERE) and len(plan["rows"]) == 180 and len(plan["command_definitions"]) == 60,
            "unexpected frozen panel scope")
    require(os.environ.get("TMPDIR") == str(collect.TEMP_ROOT) and os.environ.get("GIT_NO_REPLACE_OBJECTS") == "1",
            "home TMPDIR and Git isolation required")
    require(resource.getrlimit(resource.RLIMIT_CORE)[0] == 0, "expected unchanged core soft0")
    helper = plan["process_helper"]
    for path, digest in ((helper["source"], helper["sha256"]),
                         (PRIMARY / "collect.py", helper["collector_sha256"]),
                         (PRIMARY.parent / "prepare.py", helper["prepare_sha256"]),
                         (plan["shell"]["path"], plan["shell"]["sha256"]),
                         (plan["scheduler"]["scheduler"], plan["scheduler"]["scheduler_sha256"]),
                         (plan["scheduler"]["scheduler_binary"], plan["scheduler"]["scheduler_binary_sha256"]),
                         (plan["stdin"]["path"], plan["stdin"]["sha256"])):
        require(collect.digest(path) == digest, "input identity changed: " + str(path))
    spec = importlib.util.spec_from_file_location("existing_smoke", helper["source"])
    base = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(base)
    compilers = base.authenticate_compilers(plan)
    source = HERE / plan["source_seed"]
    inventory = json.loads((HERE / plan["source_inventory"]).read_text())
    require(inventory["inventory_sha256"] == plan["source_inventory_sha256"] and
            file_inventory(source, names(source)) == inventory, "seed membership/bytes/modes changed")
    require(not (HERE / "execution").exists() and not (HERE / "results-01").exists(), "refuse existing execution/results")
    output = HERE / "results-01"
    output.mkdir()
    env = plan["environment"]
    require(env["TMPDIR"] == str(collect.TEMP_ROOT) and env["GIT_NO_REPLACE_OBJECTS"] == "1" and
            env["KIO_CI_SCHEDULER_BIN"] == plan["scheduler"]["scheduler_binary"], "environment policy mismatch")
    definitions = {d["id"]: d["argv"] for d in plan["command_definitions"]}
    collect.save(output / "launch.json", {"commands_sha256": args.reviewed_commands_sha256,
        "script_sha256": args.reviewed_script_sha256, "compilers": compilers,
        "stack_limit": list(resource.getrlimit(resource.RLIMIT_STACK)),
        "core_limit": list(resource.getrlimit(resource.RLIMIT_CORE)),
        "measurement": "launcher-inclusive elapsed; attributed child CPU; Linux max-child-RSS, not aggregate memory"})
    baseline = allocated()
    begun = time.monotonic()
    completed = []
    state = "complete"
    def budget():
        free = shutil.disk_usage(HERE).free
        growth = allocated() - baseline
        require(free >= plan["free_space_floor_bytes"] and growth < plan["growth_budget_bytes"],
                f"resource budget reached: free={free}, packet_growth={growth}")
        return {"free_bytes": free, "packet_growth_bytes": growth}
    try:
        for row in plan["rows"]:
            if time.monotonic() - begun >= plan["dispatch_envelope_seconds"]:
                state = "stopped-dispatch-envelope"
                break
            before = budget()
            folder = HERE / "execution" / row["id"]
            require(folder.parent == HERE / "execution" and not folder.exists(), "invalid/existing row path")
            folder.mkdir(parents=True)
            copied = folder / "source"
            shutil.copytree(source, copied, copy_function=shutil.copy2)
            require(file_inventory(copied, names(copied)) == inventory, "fresh source copy mismatch")
            collect.save(folder / "source-inventory.json", inventory)
            cwd = HERE / row["cwd"]
            require(cwd.is_relative_to(copied) and cwd.is_dir(), "cwd outside fresh source")
            literal = definitions[row["command"]]
            arm = row["command"].split("/")[-1]
            require(collect.digest(literal[0]) == compilers[arm]["sha256"], "compiler changed before row")
            require(collect.digest(plan["scheduler"]["scheduler"]) == plan["scheduler"]["scheduler_sha256"] and
                    collect.digest(plan["scheduler"]["scheduler_binary"]) == plan["scheduler"]["scheduler_binary_sha256"],
                    "scheduler changed before row")
            budget()
            if time.monotonic() - begun >= plan["dispatch_envelope_seconds"]:
                state = "stopped-dispatch-envelope"
                break
            argv = [plan["shell"]["path"], plan["scheduler"]["scheduler"], "--resource", "compiler", "--", *literal]
            record = {"row": row, "process": str(folder / "capture/process.json"),
                      "status": "admitted", "before": before}
            completed.append(record)
            process, ok = base.invoke(argv, cwd, env, pathlib.Path(plan["stdin"]["path"]),
                                      folder / "capture", row["timeout_seconds"], 67108864)
            record.update({"success": ok, "status": process["status"]})
            collect.save(folder / "retained-file-inventory.json", file_inventory(copied, names(copied)))
            require(file_inventory(copied, [r["path"] for r in inventory["files"]]) == inventory,
                    "compiler modified source inputs")
            record["after"] = budget()
            print(row["id"] + (" PASS" if ok else " FAIL"), flush=True)
            if not ok:
                state = "stopped-command-failure"
                break
            if "build" in literal:
                emitted = cwd / "out" / literal[-1]
                require(emitted.is_dir() and names(emitted), "successful build produced no target files")
        budget()
        require(base.authenticate_compilers(plan) == compilers, "compiler identities changed after panel")
    except BaseException as error:
        state = "stopped-setup-resource-or-interruption"
        collect.save(output / "error.json", {"error": str(error)})
        raise
    finally:
        attempted = {r["row"]["id"] for r in completed}
        collect.save(output / "completion.json", {"state": state, "completed": completed,
            "unattempted": [r for r in plan["rows"] if r["id"] not in attempted],
            "elapsed_seconds": time.monotonic() - begun, "packet_growth_bytes": allocated() - baseline,
            "free_bytes": shutil.disk_usage(HERE).free})


if __name__ == "__main__":
    main()
