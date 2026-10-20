"""Bounded default-stack diagnostics; existing owned worker and oracles only."""
import argparse
import importlib.util
import json
import os
import pathlib
import resource
import sys
import time

HERE = pathlib.Path(__file__).resolve().parent
PRIMARY = HERE.parent
sys.path.insert(0, str(PRIMARY))
import collect
spec = importlib.util.spec_from_file_location("existing_smoke", PRIMARY / "python-smoke/run.py")
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)


def identities(rows):
    for row in rows:
        if collect.digest(row["path"]) != row["sha256"]:
            raise ValueError("identity changed: " + row["path"])


def main():
    begun = time.monotonic()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reviewed-helper-sha256", required=True)
    parser.add_argument("--reviewed-manifest-sha256", required=True)
    parser.add_argument("--reviewed-protocol-sha256", required=True)
    parser.add_argument("--reviewed-script-sha256", required=True)
    parser.add_argument("--output", required=True, type=pathlib.Path)
    args = parser.parse_args()
    for path, expected in ((PRIMARY / "python-smoke/run.py", args.reviewed_helper_sha256),
                           (HERE / "manifest.json", args.reviewed_manifest_sha256),
                           (HERE / "PROTOCOL.md", args.reviewed_protocol_sha256),
                           (pathlib.Path(__file__), args.reviewed_script_sha256)):
        if not collect.valid_digest(expected) or collect.digest(path) != expected:
            raise ValueError("reviewed launch identity mismatch: " + str(path))
    if os.environ.get("TMPDIR") != str(collect.TEMP_ROOT) or os.environ.get("GIT_NO_REPLACE_OBJECTS") != "1":
        raise ValueError("home TMPDIR and Git replacement isolation required")
    if resource.getrlimit(resource.RLIMIT_CORE)[0] != 0:
        raise ValueError("expected unchanged core soft limit0; do not modify limits here")
    for name in ("GHCRTS", "JAVA_TOOL_OPTIONS", "_JAVA_OPTIONS", "JDK_JAVA_OPTIONS", "GODEBUG"):
        if os.environ.get(name):
            raise ValueError("unexpected inherited runtime policy: " + name)
    manifest = json.loads((HERE / "manifest.json").read_text())
    if len(manifest["probes"]) != 240 or manifest["depths"] != [64, 256, 1024] or manifest["global_envelope_seconds"] != 1200:
        raise ValueError("unexpected scope")
    identities(manifest["sources"])
    for artifact in manifest["artifacts"].values():
        identities(artifact["files"] + [artifact["stdin"]])
    output = args.output.resolve()
    if not output.is_relative_to(collect.TEMP_ROOT.resolve()) or output == collect.TEMP_ROOT.resolve():
        raise ValueError("output outside approved temporary root")
    output.mkdir(exist_ok=False)
    collect.save(output / "launch.json", {"manifest_sha256": args.reviewed_manifest_sha256,
        "protocol_sha256": args.reviewed_protocol_sha256, "script_sha256": args.reviewed_script_sha256,
        "stack_limit": list(resource.getrlimit(resource.RLIMIT_STACK)), "core_limit": list(resource.getrlimit(resource.RLIMIT_CORE)),
        "global_envelope_seconds": 1200, "maximum_probes": 240,
        "note": "Depth diagnostics only; no speed ratios/stack-byte/heap/adoption conclusion."})
    completed, failed, maxima = [], {}, {}
    backend_order = ("python", "quickjs", "rust", "go", "java", "swift", "haskell")
    mode_order = ("tail", "pending", "callback-tail", "callback-pending")
    arm_order = ("C", "S", "B", "P", "T")
    def order(probe):
        c = probe["input"]
        return (c["depth"], backend_order.index(probe["backend"]), mode_order.index(c["mode"]), arm_order.index(c["arm"]))
    probes = sorted(manifest["probes"], key=order)
    state = "complete"
    try:
        for probe in probes:
            config = probe["input"]
            key = probe["backend"] + ":" + config["arm"] + ":" + config["mode"]
            record = {"backend": probe["backend"], "input": config, "key": key}
            if key in failed:
                completed.append({**record, "status": "not-run-known-failure", "first_failed_depth": failed[key]["depth"]})
                continue
            if time.monotonic() - begun > 1135:
                completed.append({**record, "status": "not-run-global-envelope"})
                state = "stopped-global-envelope"
                continue
            artifact = manifest["artifacts"][probe["artifact"]]
            identities(manifest["sources"] + artifact["files"] + [artifact["stdin"], probe["request"]] + ([probe["config_file"]] if probe["config_file"] else []))
            request = json.loads(pathlib.Path(probe["request"]["path"]).read_text())
            if request["stdin"] != artifact["stdin"]["path"] or request["timeout"] != 60 or request["cap"] != 1048576:
                raise ValueError("request policy mismatch")
            if time.monotonic() - begun > 1135:
                completed.append({**record, "status": "not-run-global-envelope"})
                state = "stopped-global-envelope"
                continue
            slot = output / f"{probe['backend']}-{config['mode']}-{config['arm']}-depth{config['depth']}"
            process, ok = base.invoke(request["command"]["argv"], pathlib.Path(request["command"]["cwd"]),
                request["env"], pathlib.Path(request["stdin"]), slot, 60, 1048576)
            identities(manifest["sources"] + artifact["files"] + [artifact["stdin"], probe["request"]] + ([probe["config_file"]] if probe["config_file"] else []))
            if process["status"] == "interrupted":
                raise ValueError("runtime interrupted; stop whole protocol: " + str(slot))
            if process["status"] == "launch-error":
                raise ValueError("runtime launch/setup error: " + str(slot))
            if ok:
                driver = json.loads((slot / "stdout.bin").read_text())
                kind = "targeted" if probe["backend"] in collect.TARGETED_PANELS else "primary"
                errors = collect.driver_errors({"kind": kind, "input": {k: v for k, v in config.items() if k != "arm"},
                    "backend": probe["backend"]}, {"id": config["arm"]}, driver)
                if process["stderr"]["bytes"]:
                    errors.append("successful probe requires empty stderr")
                collect.save(slot / "oracle-check.json", {"input": config, "driver": driver, "errors": errors})
                if errors:
                    raise ValueError("semantic/oracle/policy mismatch: " + str(slot))
                maxima[key] = config["depth"]
                status = "success"
            else:
                status = "censored" if process["censored"] else "runtime-failure-unclassified"
                failed[key] = {"depth": config["depth"], "status": status, "returncode": process["returncode"], "process": str(slot / "process.json")}
            collect.save(slot / "diagnostic.json", {**record, "status": status,
                "highest_completed_depth_in_this_ladder": maxima.get(key),
                "note": "Nonzero/censoring does not by itself prove stack exhaustion; inspect preserved native stderr/status. No speed ratio."})
            completed.append({**record, "status": status, "process": str(slot / "process.json")})
        collect.save(output / "completion.json", {"state": state, "completed": completed, "first_failures": failed,
            "highest_completed_depths_in_this_ladder": maxima, "elapsed_seconds": time.monotonic() - begun})
    except BaseException as error:
        collect.save(output / "completion.json", {"state": "stopped-protocol-or-interruption", "completed": completed,
            "first_failures": failed, "highest_completed_depths_in_this_ladder": maxima,
            "elapsed_seconds": time.monotonic() - begun, "error": str(error)})
        raise


if __name__ == "__main__":
    main()
