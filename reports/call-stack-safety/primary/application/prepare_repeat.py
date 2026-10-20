"""Five reviewed driver links and fixed witnesses; optional separate calibration."""
import importlib.util
import json
import pathlib
import os
import sys
HERE = pathlib.Path(__file__).resolve().parent
PRIMARY = HERE.parent
sys.path.insert(0, str(PRIMARY))
import collect
spec = importlib.util.spec_from_file_location("existing_smoke", PRIMARY / "python-smoke/run.py")
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
env = json.loads((HERE / "preparation-environment.json").read_text())["env_allowlist"]
source = pathlib.Path(os.environ["KIO_DEBUG_CALL_STACK_COMPILER_SOURCE"])
stdin = pathlib.Path(json.loads((PRIMARY / "compiler-cost-secondary/commands.json").read_text())["stdin"]["path"])
transformed = HERE / "repeat-source"
calibration = sys.argv[1:] == ["--calibration"]
assert not sys.argv[1:] or calibration

def validate(folder, repeat, warmup):
    process = json.loads((folder / "process.json").read_text())
    assert process["returncode"] == 0 and process["status"] == "exited" and not process["censored"] and not process["stderr"]["bytes"]
    actual = json.loads((folder / "stdout.bin").read_text())
    assert set(actual) == {"schema", "repeat", "warmup", "invoke_ns", "loop_ns", "checked_bytes", "fnv1a64"}
    assert actual["schema"] == "order-book-repeat-v1" and actual["repeat"] == repeat and actual["warmup"] == warmup
    for key in ("repeat", "warmup", "invoke_ns", "loop_ns", "checked_bytes"):
        assert type(actual[key]) is int and actual[key] >= 0
    assert actual["loop_ns"] >= actual["invoke_ns"] and actual["checked_bytes"] == 2509 * repeat
    oracle = transformed / "B/order-book.expected.stdout"
    assert collect.digest(oracle) == "d8881702601729da0bcc656c46fa600383f067fc0f5ff28b0495c51eff4f4d56"
    checksum = 0xcbf29ce484222325
    for _ in range(repeat):
        for byte in oracle.read_bytes():
            checksum = ((checksum ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
    assert actual["fnv1a64"] == f"{checksum:016x}"
    collect.save(folder / "oracle-check.json", {"errors": [], "repeat": repeat, "warmup": warmup,
        "expected_checked_bytes": 2509 * repeat, "expected_fnv1a64": f"{checksum:016x}"})
    return actual

rows = []
for arm in collect.ARMS:
    original = HERE / "native" / arm / "order_book_matcher"
    snapshot = original / "snapshot"
    root = HERE / "repeat-native" / arm
    binary = root / "application-bin"
    driver = transformed / arm / "driver.rs"
    assert collect.digest(driver) == "55f8102d2ccfc029033ba5d6d419cf4093681a5eabb14f09cdf26dfed7127de3"
    if not calibration:
        root.mkdir(parents=True)
        receipt = json.loads((original / "preparation.json").read_text())
        assert receipt["runtime_success"]
        commands = [json.loads(pathlib.Path(c["request"]).read_text()) for c in receipt["native_commands"]]
        request = next(c for c in commands if c["source"] is not None)
        assert collect.digest(snapshot / "driver.rs") == request["source"]["sha256"] == "e6082a5fbec2ee264f91c3dc78ef8f962d21d270e1fff61cc68bcc0987d26a14"
        meta = json.loads((snapshot / "snapshot.json").read_text())
        rlib = snapshot / "liborder_book_matcher.rlib"
        assert collect.digest(rlib) == meta["files"][rlib.name]["sha256"]
        argv = list(request["argv"])
        extern = argv.index("--extern") + 1
        output = argv.index("-o") + 1
        assert argv[extern].startswith("order_book_matcher=") and pathlib.Path(argv[-1]).name == "driver.rs"
        changed = {extern: "order_book_matcher=" + str(rlib), output: str(binary), len(argv)-1: str(driver)}
        for index, value in changed.items():
            argv[index] = value
        assert [i for i, (a,b) in enumerate(zip(argv, request["argv"])) if a != b] == sorted(changed)
        collect.save(root / "link-command.json", {"original": request, "argv": argv, "cwd": request["cwd"],
            "changed_indices": sorted(changed), "rlib_sha256": collect.digest(rlib), "driver_sha256": collect.digest(driver)})
        command = ["sh", str(source / "ci/impl-toolchain.sh"), "toolchain-exec", "rust", "--core", "--",
            "sh", str(source / "ci/schedule.sh"), "--resource", "compiler", "--", *argv]
        process, ok = base.invoke(command, pathlib.Path(request["cwd"]), env, stdin, root / "link", 600, 67108864)
        if not ok:
            raise ValueError("preserved optional driver-link failure: " + arm)
        collect.save(root / "binary.json", {"path": str(binary), "sha256": collect.digest(binary), "bytes": binary.stat().st_size,
            "mode": oct(binary.stat().st_mode & 0o777), "source_sha256": collect.digest(driver)})
    identity = json.loads((root / "binary.json").read_text())
    assert collect.digest(binary) == identity["sha256"]
    for repeat, warmup in ([(1000, 0)] if calibration else [(2, 0), (2, 1)]):
        folder = root / (f"calibration-{repeat}-{warmup}" if calibration else f"witness-{repeat}-{warmup}")
        process, ok = base.invoke([str(binary), str(repeat), str(warmup)], root, env, stdin, folder, 60, 1048576)
        if not ok:
            raise ValueError("preserved repeated-entry runtime failure: " + str(folder))
        actual = validate(folder, repeat, warmup)
        rows.append({"arm": arm, "repeat": repeat, "warmup": warmup, "driver": actual, "process": str(folder / "process.json")})
    print(arm + (" common calibration PASS" if calibration else " optional link + two witnesses PASS"), flush=True)
collect.save(HERE / ("repeat-calibration-completion.json" if calibration else "repeat-link-witness-completion.json"),
    {"state": "complete", "rows": rows, "note": "No balanced timing panel or count selection executed"})
