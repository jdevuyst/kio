"""Bounded Linux collector. Execution requires an explicitly trusted frozen argv file.

This is separate from prepare.py: its manifest checker remains offline-only.
Selfcheck executes only small fixed Python fixtures, never Kio or host drivers.
"""

import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import resource
import signal
import subprocess
import sys
import tempfile
import time

ROOT = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
from oracle import config_errors, result_errors  # noqa: E402
from prepare import ARMS, HEX40, HEX64, git_read, schedule  # noqa: E402

TEMP_ROOT = pathlib.Path(os.environ["TMPDIR"]).resolve(strict=True)
ENV_KEYS = {"PATH", "LANG", "LC_ALL", "LC_CTYPE", "TZ", "TMPDIR", "GIT_NO_REPLACE_OBJECTS", "PYTHONDONTWRITEBYTECODE", "LD_LIBRARY_PATH"}
IDENTITIES = ("compiler_sha256", "build_receipt_sha256", "toolchain_receipt_sha256", "driver_source_inventory_sha256", "workload_source_inventory_sha256")
TARGETED_PANELS = {"go": "targeted", "java": "targeted", "swift": "swift-targeted", "haskell": "haskell-targeted"}
_targeted_validators = {}


def targeted_validator(backend):
    panel = TARGETED_PANELS[backend]
    if panel not in _targeted_validators:
        spec = importlib.util.spec_from_file_location("collector_" + panel.replace("-", "_"),
                                                     ROOT / panel / "check.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        _targeted_validators[panel] = module
    return _targeted_validators[panel]


def targeted_config_errors(backend, config):
    if not isinstance(backend, str) or backend not in TARGETED_PANELS:
        return ["unknown targeted backend"]
    if not isinstance(config, dict):
        return ["targeted input must be an object"]
    validator = targeted_validator(backend)
    mode = config.get("mode")
    if not isinstance(mode, str):
        return ["targeted mode must be a string"]
    if backend in ("go", "java"):
        modes = validator.primary.MODES | ({"callback-extract"} if backend == "go" else set())
        projected = {**config, "mode": "callback-tail" if mode == "callback-extract" else mode}
    else:
        modes = validator.MODES
        projected = validator.core_config(config)
    errors = [] if mode in modes else ["unknown mode for targeted backend"]
    return errors + validator.primary.config_errors(projected)


def driver_errors(doc, arm, driver):
    config = {**doc["input"], "arm": arm["id"]}
    if doc["kind"] == "targeted":
        errors = targeted_config_errors(doc["backend"], config)
        if errors:
            return errors
        validator = targeted_validator(doc["backend"])
        errors = (validator.result_errors(doc["backend"], config, driver)
                  if doc["backend"] in ("go", "java") else validator.result_errors(config, driver))
    else:
        errors = result_errors(config, driver)
    if not isinstance(driver, dict) or driver.get("backend") != doc["backend"]:
        errors.append("driver backend differs from the frozen cell")
    return errors


def digest(path):
    h = hashlib.sha256()
    with pathlib.Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def save(path, value):
    with pathlib.Path(path).open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.write("\n")


def valid_digest(value, pattern=HEX64):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def validate(doc, verify_files=False):
    errors = []
    if not isinstance(doc, dict):
        return ["trusted command file must be an object"]
    if doc.get("schema") != "kio-trusted-runtime-cell-v1" or doc.get("state") != "reviewed-commands-frozen":
        errors.append("command file is not reviewed and frozen")
    if doc.get("kind") not in ("primary", "targeted", "corpus-process"):
        errors.append("unknown cell kind")
    if not isinstance(doc.get("cell"), str) or not doc["cell"]:
        errors.append("missing logical cell id")
    if not isinstance(doc.get("profile"), str) or not doc["profile"]:
        errors.append("missing artifact profile")
    if not isinstance(doc.get("backend"), str) or not doc["backend"]:
        errors.append("missing backend identity")
    if type(doc.get("blocks")) is not int or doc["blocks"] not in (5, 10):
        errors.append("use 5 pilot blocks or 10 complete blocks")
    if type(doc.get("schedule_seed")) is not int or not 0 <= doc["schedule_seed"] <= 4294967295:
        errors.append("invalid schedule seed")
    if type(doc.get("timeout_seconds")) not in (int, float) or not 0 < doc["timeout_seconds"] <= 600:
        errors.append("timeout must be in (0,600]")
    if type(doc.get("max_output_bytes")) is not int or not 1024 <= doc["max_output_bytes"] <= 1048576:
        errors.append("output safety cap must be 1KiB..1MiB per stream")
    if doc.get("expected_exit") != 0 or type(doc.get("expected_exit")) is not int:
        errors.append("successful runtime cells require exact exit 0")
    if doc.get("stderr_policy") not in ("empty", "record-only"):
        errors.append("stderr policy must be explicit")
    if doc.get("kind") in ("primary", "targeted"):
        targeted = doc["kind"] == "targeted"
        if not targeted and doc.get("backend") not in ("quickjs", "python", "rust"):
            errors.append("unknown primary backend")
        if not isinstance(doc.get("input"), dict) or "arm" in doc["input"]:
            errors.append("common input must omit only the arm field")
        else:
            config = {**doc["input"], "arm": "B"}
            errors += targeted_config_errors(doc.get("backend"), config) if targeted else config_errors(config)
            if doc["input"].get("purpose") != "time":
                errors.append("balanced timing cells require time purpose; witnesses run separately")
    elif not valid_digest(doc.get("expected_stdout_sha256")):
        errors.append("corpus process cell needs an exact independent stdout hash")
    env = doc.get("env")
    if not isinstance(env, dict) or set(env) - ENV_KEYS or any(not isinstance(v, str) or "\0" in v for v in env.values()):
        errors.append("env must contain only declared non-secret string settings")
    elif env.get("TMPDIR") != str(TEMP_ROOT) or env.get("GIT_NO_REPLACE_OBJECTS") != "1":
        errors.append("exact home TMPDIR and disabled Git replacement objects required")
    if not valid_digest(doc.get("protocol_receipt_sha256")) or not valid_digest(doc.get("review_receipt_sha256")):
        errors.append("protocol/review receipt hashes required")
    stdin = doc.get("stdin")
    if not isinstance(stdin, dict) or not valid_digest(stdin.get("sha256")) or not isinstance(stdin.get("path"), str):
        errors.append("an explicit frozen stdin file/hash is required, including for empty input")
    elif verify_files:
        path = pathlib.Path(stdin["path"])
        if not path.is_absolute() or not path.is_file() or path.stat().st_size > 1048576 or digest(path) != stdin["sha256"]:
            errors.append("stdin identity/bounds mismatch")
    arms = doc.get("arms")
    if not isinstance(arms, list) or len(arms) != 5 or any(not isinstance(a, dict) for a in arms):
        return errors + ["exactly five arm records required"]
    if [a.get("id") for a in arms] != ARMS:
        errors.append("arms must be ordered B,T,C,S,P")
    pinned = {a["id"]: a["commit"] for a in json.loads((ROOT / "inventory.json").read_text())["arms"]}
    for arm in arms:
        label = str(arm.get("id"))
        if arm.get("source_commit") != pinned.get(arm.get("id")):
            errors.append(label + ": source differs from the exact reviewed five-arm inventory")
        if any(not valid_digest(arm.get(key), HEX40) for key in ("source_commit", "source_tree")):
            errors.append(label + ": exact source commit/tree required")
        if any(not valid_digest(arm.get(key)) for key in IDENTITIES):
            errors.append(label + ": missing compiler/tool/build/driver/workload identity")
        argv = arm.get("argv")
        if not isinstance(argv, list) or not argv or any(not isinstance(a, str) or "\0" in a for a in argv) or not pathlib.Path(argv[0]).is_absolute():
            errors.append(label + ": trusted argv must start with an absolute executable")
        if not isinstance(arm.get("cwd"), str) or not pathlib.Path(arm["cwd"]).is_absolute():
            errors.append(label + ": absolute cwd required")
        files = arm.get("files")
        if not isinstance(files, list) or not files:
            errors.append(label + ": executable and artifact file identities required")
            continue
        paths = []
        for item in files:
            if not isinstance(item, dict) or not isinstance(item.get("path"), str) or not pathlib.Path(item["path"]).is_absolute() or not valid_digest(item.get("sha256")):
                errors.append(label + ": invalid frozen file identity")
                continue
            paths.append(item["path"])
            if verify_files and (not pathlib.Path(item["path"]).is_file() or digest(item["path"]) != item["sha256"]):
                errors.append(label + ": file content changed: " + item["path"])
        if len(paths) != len(set(paths)) or isinstance(argv, list) and argv and argv[0] not in paths:
            errors.append(label + ": duplicate files or executable missing from inventory")
    return errors


def sample(command, env, stdin, output, timeout, cap):
    """One child, called only inside a fresh worker (or fixed synthetic selfcheck)."""
    if sys.platform != "linux":
        raise ValueError("this resource collector is Linux-only")
    stdout = output / "stdout.bin"
    stderr = output / "stderr.bin"
    usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)

    def limits():
        # Cap redirected output/storage, not stack/heap or CPU speed.
        resource.setrlimit(resource.RLIMIT_FSIZE, (cap, cap))

    status = "exited"
    error = None
    code = None
    proc = None
    waiting = False
    interrupted = False

    def request_stop(_signum, _frame):
        nonlocal interrupted
        interrupted = True
        # Latch during spawn; never asynchronously abandon a half-created Popen.
        if waiting:
            signal.setitimer(signal.ITIMER_REAL, 0.001)

    previous_handlers = {sig: signal.signal(sig, request_stop) for sig in (signal.SIGINT, signal.SIGTERM)}

    def stop_group():
        # Do not reap the leader before escalation: its exit is not evidence
        # that same-group descendants stopped, and retaining it avoids PID reuse.
        try:
            os.killpg(proc.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        time.sleep(0.25)
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        return proc.wait()

    start = time.perf_counter_ns()
    with stdout.open("xb") as out, stderr.open("xb") as err, pathlib.Path(stdin).open("rb") as inp:
        try:
            proc = subprocess.Popen(command["argv"], cwd=command["cwd"], env=env,
                                    stdin=inp, stdout=out, stderr=err, shell=False,
                                    start_new_session=True, preexec_fn=limits)
            try:
                # WNOWAIT retains the leader's identity until failure cleanup
                # is decided. Blocking avoids short-sample polling overhead.
                def alarm(_signum, _frame):
                    raise subprocess.TimeoutExpired(command["argv"], timeout)
                previous_handler = signal.signal(signal.SIGALRM, alarm)
                waiting = True
                signal.setitimer(signal.ITIMER_REAL, timeout)
                try:
                    if interrupted:
                        raise subprocess.TimeoutExpired(command["argv"], timeout)
                    info = os.waitid(os.P_PID, proc.pid, os.WEXITED | os.WNOWAIT)
                    code = info.si_status if info.si_code == os.CLD_EXITED else -info.si_status
                finally:
                    waiting = False
                    signal.setitimer(signal.ITIMER_REAL, 0)
                    signal.signal(signal.SIGALRM, previous_handler)
                if interrupted:
                    status = "interrupted"
                    code = stop_group()
                elif code != 0 or stdout.stat().st_size >= cap or stderr.stat().st_size >= cap:
                    if code == -signal.SIGXFSZ or stdout.stat().st_size >= cap or stderr.stat().st_size >= cap:
                        status = "output-limit"
                    code = stop_group()
                else:
                    code = proc.wait()
            except subprocess.TimeoutExpired:
                status = "interrupted" if interrupted else "timeout"
                code = stop_group()
        except OSError as exc:
            status, error = "launch-error", str(exc)
            if proc is not None and proc.returncode is None:
                code = stop_group()
        finally:
            wall_ns = time.perf_counter_ns() - start
            for sig, handler in previous_handlers.items():
                signal.signal(sig, handler)
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return {
        "status": status, "returncode": code, "launch_error": error,
        "wall_ns": wall_ns, "timeout_seconds": timeout, "max_output_bytes": cap,
        "censored": status in ("timeout", "output-limit"),
        "child_user_cpu_seconds": usage.ru_utime - usage_before.ru_utime,
        "child_system_cpu_seconds": usage.ru_stime - usage_before.ru_stime,
        "max_child_rss_bytes": usage.ru_maxrss * 1024,
        "resource_scope": "fresh Linux supervisor: CPU for reaped child/attributed descendants; RSS is maximum child high-water mark, not summed process-tree/live heap",
        "stack_limit": list(resource.getrlimit(resource.RLIMIT_STACK)),
        "stdout": {"path": str(stdout), "bytes": stdout.stat().st_size, "sha256": digest(stdout)},
        "stderr": {"path": str(stderr), "bytes": stderr.stat().st_size, "sha256": digest(stderr)},
    }


def owned_worker(output):
    """Keep the owned worker alive until it has cleaned up and emitted a receipt."""
    worker = None
    interrupted = False

    def request_stop(_signum, _frame):
        nonlocal interrupted
        interrupted = True
        if worker is not None:
            try:
                worker.send_signal(signal.SIGINT)
            except ProcessLookupError:
                pass

    previous_handlers = {sig: signal.signal(sig, request_stop) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        worker = subprocess.Popen([sys.executable, "-B", str(pathlib.Path(__file__).resolve()), "_sample", str(output)],
                                  shell=False, start_new_session=True)
        if interrupted:
            request_stop(None, None)
        code = worker.wait()
        return code, interrupted
    finally:
        for sig, handler in previous_handlers.items():
            signal.signal(sig, handler)


def check_success(doc, arm, row):
    if row["status"] != "exited":
        return row["status"], []
    if row["returncode"] != doc["expected_exit"]:
        return "exit-failure", []
    errors = []
    if doc["stderr_policy"] == "empty" and row["stderr"]["bytes"]:
        errors.append("nonempty stderr")
    if doc["kind"] in ("primary", "targeted"):
        try:
            driver = json.loads(pathlib.Path(row["stdout"]["path"]).read_text())
            errors += driver_errors(doc, arm, driver)
            row["driver_phases"] = driver
        except (OSError, ValueError, TypeError) as exc:
            errors.append("invalid driver result: " + str(exc))
    elif row["stdout"]["sha256"] != doc["expected_stdout_sha256"]:
        errors.append("independent corpus stdout mismatch")
    return ("oracle-mismatch" if errors else "success"), errors


def run_cell(path, approved, output, repo):
    if sys.platform != "linux" or os.environ.get("TMPDIR") != str(TEMP_ROOT):
        raise ValueError("Linux and the exact approved TMPDIR are required")
    if not valid_digest(approved) or digest(path) != approved:
        raise ValueError("pass the exact independently reviewed trusted-command file SHA256")
    doc = json.loads(path.read_text())
    errors = validate(doc, verify_files=True)
    if errors:
        raise ValueError("; ".join(errors))
    for arm in doc["arms"]:
        if git_read(repo, "rev-parse", arm["source_commit"] + "^{tree}") != arm["source_tree"]:
            raise ValueError("source tree does not match its exact commit")
    resolved = output.resolve()
    if not resolved.is_relative_to(TEMP_ROOT.resolve()) or resolved == TEMP_ROOT.resolve():
        raise ValueError("new output directory must be below the approved temporary root")
    output.mkdir(parents=False, exist_ok=False)
    orders = schedule(doc["schedule_seed"])[:doc["blocks"]]
    save(output / "cell.json", {"trusted_command_sha256": approved, "commands": doc, "blocks": orders,
                                 "collector_sha256": digest(__file__), "supervisor_executable": sys.executable,
                                 "supervisor_sha256": digest(sys.executable), "state": "running"})
    failed = {}
    completed = []
    for block, order in enumerate(orders):
        for position, name in enumerate(order):
            arm = next(a for a in doc["arms"] if a["id"] == name)
            slot = output / f"block-{block:02d}-{position}-{name}"
            slot.mkdir()
            base = {"cell": doc["cell"], "backend": doc["backend"], "profile": doc["profile"],
                    "block": block, "position": position, "arm": arm,
                    "trusted_command_sha256": approved, "stdin": doc["stdin"], "env": doc["env"],
                    "expected_exit": doc["expected_exit"], "expected_input": doc.get("input"),
                    "expected_stdout_sha256": doc.get("expected_stdout_sha256"),
                    "protocol_receipt_sha256": doc["protocol_receipt_sha256"], "review_receipt_sha256": doc["review_receipt_sha256"]}
            if name in failed:
                save(slot / "result.json", {**base, "status": "not-run-known-failure", "earlier_status": failed[name]})
                continue
            stat = os.statvfs(TEMP_ROOT)
            if stat.f_bavail * stat.f_frsize < 10 * 1024**3:
                save(slot / "result.json", {**base, "status": "not-run-disk-headroom"})
                save(output / "completion.json", {"state": "stopped-disk-headroom", "completed": completed})
                return 2
            errors = validate(doc, verify_files=True)
            if errors:
                save(slot / "result.json", {**base, "status": "identity-mismatch", "errors": errors})
                save(output / "completion.json", {"state": "stopped-identity", "completed": completed})
                return 2
            request = {"command": arm, "env": doc["env"], "stdin": doc["stdin"]["path"],
                       "timeout": doc["timeout_seconds"], "cap": doc["max_output_bytes"]}
            save(slot / "request.json", request)
            worker_code, interrupted = owned_worker(slot)
            if worker_code or not (slot / "process.json").is_file():
                save(slot / "result.json", {**base, "status": "supervisor-interrupted" if interrupted else "supervisor-failure", "returncode": worker_code})
                save(output / "completion.json", {"state": "stopped-supervisor", "completed": completed})
                return 2
            row = json.loads((slot / "process.json").read_text())
            status, errors = check_success(doc, arm, row)
            if interrupted:
                status = "interrupted"
            row.update(base)
            row.update(status=status, oracle_errors=errors)
            # A changing executable/artifact invalidates even an apparently valid row.
            identity_errors = validate(doc, verify_files=True)
            if identity_errors:
                row.update(status="identity-mismatch", identity_errors=identity_errors)
                status = "identity-mismatch"
            save(slot / "result.json", row)
            completed.append({"block": block, "position": position, "arm": name, "status": status})
            if status in ("oracle-mismatch", "identity-mismatch", "interrupted", "launch-error"):
                save(output / "completion.json", {"state": "stopped-invalid-block", "invalid_block": block, "completed": completed})
                return 2
            if status != "success":
                failed[name] = status
    save(output / "completion.json", {"state": "complete-with-failures" if failed else "complete",
                                      "failed_arms": failed, "completed": completed,
                                      "note": "failed/censored arms have no speedup ratio; no performance acceptance decision is made"})
    return 1 if failed else 0


def selfcheck():
    if not __debug__:
        raise ValueError("selfcheck requires assertions")
    assert validate({})
    env = {"TMPDIR": str(TEMP_ROOT), "GIT_NO_REPLACE_OBJECTS": "1", "PYTHONDONTWRITEBYTECODE": "1"}
    mock = json.loads((ROOT / "trusted-cell.template.json").read_text())
    mock.update(env=env, state="reviewed-commands-frozen", protocol_receipt_sha256="1" * 64,
                review_receipt_sha256="2" * 64, stdin={"path": "/synthetic-stdin", "sha256": "3" * 64})
    for arm in mock["arms"]:
        arm.update({key: "4" * 64 for key in IDENTITIES})
        arm.update(argv=[sys.executable], cwd="/synthetic-cwd",
                   files=[{"path": sys.executable, "sha256": "5" * 64}])
    assert validate(mock) == []  # Structural mocks are never execution authority.
    assert validate({**mock, "env": {**env, "SECRET_TOKEN": "not-allowed"}})
    assert validate({**mock, "timeout_seconds": float("nan")})
    mock["arms"][0]["source_commit"] = "0" * 40
    assert validate(mock)
    # No Kio, host toolchain, generated package, driver, cache or network involved.
    with tempfile.TemporaryDirectory(prefix="collector-selfcheck.", dir=TEMP_ROOT) as tmp:
        root = pathlib.Path(tmp)
        stdin = root / "stdin"
        stdin.write_bytes(b"")
        for label, argv, timeout, expected in (
            ("ok", [sys.executable, "-B", "-c", "print('synthetic')"], 2, "exited"),
            ("exit", [sys.executable, "-B", "-c", "raise SystemExit(7)"], 2, "exited"),
            ("timeout", [sys.executable, "-B", "-c", "import time; time.sleep(2)"], 0.03, "timeout"),
            ("literal", [sys.executable, "-B", "-c", "import sys; print(sys.argv[1])", ";touch should-not-exist"], 2, "exited"),
            ("output", [sys.executable, "-B", "-c", "import os; os.write(1, b'x' * 8192)"], 2, "output-limit"),
        ):
            slot = root / label
            slot.mkdir()
            save(slot / "request.json", {"command": {"argv": argv, "cwd": str(root)}, "env": env,
                                         "stdin": str(stdin), "timeout": timeout, "cap": 4096})
            subprocess.run([sys.executable, "-B", str(pathlib.Path(__file__).resolve()), "_sample", str(slot)], check=True)
            row = json.loads((slot / "process.json").read_text())
            assert row["status"] == expected and row["wall_ns"] > 0
            assert row["max_child_rss_bytes"] > 0
            if label == "exit":
                assert row["returncode"] == 7
            if label == "ok":
                oracle_doc = {"kind": "corpus-process", "expected_exit": 0, "stderr_policy": "empty",
                              "expected_stdout_sha256": hashlib.sha256(b"synthetic\n").hexdigest()}
                assert check_success(oracle_doc, {"id": "B"}, row) == ("success", [])
                assert check_success({**oracle_doc, "expected_stdout_sha256": "0" * 64}, {"id": "B"}, row)[0] == "oracle-mismatch"
            if label in ("timeout", "output"):
                assert row["censored"]
            if label == "literal":
                assert (slot / "stdout.bin").read_text() == ";touch should-not-exist\n"
                assert not (root / "should-not-exist").exists()
        lifecycle_selfcheck(root, stdin, env)
        print(json.dumps({"status": "synthetic process/timeout/argv/owned-cleanup selfchecks passed", "real_driver_runs": 0}))


def lifecycle_selfcheck(root, stdin, env):
    # The grandchild stays in its leader's group but deliberately ignores TERM.
    # Every fixture also exits on its own after three seconds if a check fails.
    child = "import os,signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); print('child',os.getpid(),flush=True); time.sleep(3)"
    leader = "import os,subprocess,sys,time; print('leader',os.getpid(),flush=True); subprocess.Popen([sys.executable,'-B','-c'," + repr(child) + "]); time.sleep(3)"
    ready_leader = "import os,signal,subprocess,sys; print('leader',os.getpid(),flush=True); child=subprocess.Popen([sys.executable,'-B','-c'," + repr(child) + "],stdout=subprocess.PIPE,text=True); print(child.stdout.readline(),end='',flush=True); "
    output_leader = ready_leader + "signal.signal(signal.SIGXFSZ,signal.SIG_DFL); os.write(1,b'x'*8192); os.write(1,b'x'); child.wait()"
    failed_leader = ready_leader + "raise SystemExit(7)"

    def pids(slot):
        path = slot / "stdout.bin"
        return {parts[0]: int(parts[1]) for line in path.read_text().splitlines()
                if len(parts := line.split()) == 2 and parts[0] in ("leader", "child") and parts[1].isdigit()} if path.exists() else {}

    def alive(pid):
        try:
            # An orphan may briefly await init's reap; a zombie is not live work.
            return pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[0] != "Z"
        except FileNotFoundError:
            return False

    for label, stop, target, timeout, expected in (
        ("group-timeout", None, leader, 0.5, "timeout"),
        ("owned-int", signal.SIGINT, leader, 2, "interrupted"),
        ("owned-term", signal.SIGTERM, leader, 2, "interrupted"),
        ("group-output", None, output_leader, 2, "output-limit"),
        ("group-exit", None, failed_leader, 2, "exited"),
    ):
        slot = root / label
        slot.mkdir()
        save(slot / "request.json", {"command": {"argv": [sys.executable, "-B", "-c", target], "cwd": str(root)},
                                     "env": env, "stdin": str(stdin), "timeout": timeout, "cap": 4096})
        outer = subprocess.Popen([sys.executable, "-B", str(pathlib.Path(__file__).resolve()), "_owned-sample", str(slot)],
                                 shell=False, start_new_session=True)
        known = {}
        try:
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                known = pids(slot)
                if set(known) == {"leader", "child"}:
                    break
                if outer.poll() is not None:
                    break
                time.sleep(0.01)
            assert set(known) == {"leader", "child"}, (label, "fixture did not become ready")
            if stop is not None:
                outer.send_signal(stop)
            assert outer.wait(timeout=3) == 0
            row = json.loads((slot / "process.json").read_text())
            ownership = json.loads((slot / "ownership.json").read_text())
            assert row["status"] == expected, row
            if label == "group-exit":
                assert row["returncode"] == 7
            assert ownership == {"worker_returncode": 0, "interrupted": stop is not None}
            deadline = time.monotonic() + 0.5
            while any(alive(pid) for pid in known.values()) and time.monotonic() < deadline:
                time.sleep(0.01)
            assert not any(alive(pid) for pid in known.values()), (label, "known target remained live", known)
        finally:
            # Exact fixture-owned groups only; no unrelated process matching.
            known.update(pids(slot))
            if known.get("leader") and any(alive(pid) for pid in known.values()):
                try:
                    os.killpg(known["leader"], signal.SIGKILL)
                except ProcessLookupError:
                    pass
            if outer.poll() is None:
                outer.send_signal(signal.SIGTERM)
                try:
                    outer.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    outer.kill()
                    outer.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("selfcheck")
    check = commands.add_parser("check-trusted")
    check.add_argument("path", type=pathlib.Path)
    run = commands.add_parser("run-reviewed")
    run.add_argument("path", type=pathlib.Path)
    run.add_argument("--reviewed-sha256", required=True)
    run.add_argument("--output", required=True, type=pathlib.Path)
    run.add_argument("--repo", required=True, type=pathlib.Path)
    worker = commands.add_parser("_sample")
    worker.add_argument("output", type=pathlib.Path)
    owned = commands.add_parser("_owned-sample")
    owned.add_argument("output", type=pathlib.Path)
    args = parser.parse_args()
    if args.command == "selfcheck":
        selfcheck()
    elif args.command == "check-trusted":
        errors = validate(json.loads(args.path.read_text()))
        print(json.dumps({"complete": not errors, "errors": errors}, indent=2))
        return bool(errors)
    elif args.command == "run-reviewed":
        return run_cell(args.path, args.reviewed_sha256, args.output, args.repo)
    elif args.command == "_owned-sample":
        code, interrupted = owned_worker(args.output)
        save(args.output / "ownership.json", {"worker_returncode": code, "interrupted": interrupted})
        return code
    else:
        request = json.loads((args.output / "request.json").read_text())
        row = sample(request["command"], request["env"], request["stdin"], args.output, request["timeout"], request["cap"])
        save(args.output / "process.json", row)
    return 0


if __name__ == "__main__":
    sys.exit(main())
