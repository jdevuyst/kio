#!/usr/bin/env python3
"""Offline preparation only: never execute manifest commands or performance work."""

import argparse
import collections
import hashlib
import json
import pathlib
import random
import re
import subprocess
import sys

ARMS = ["B", "T", "C", "S", "P"]
HEX40 = re.compile(r"[0-9a-f]{40}\Z")
HEX64 = re.compile(r"[0-9a-f]{64}\Z")


def emit(value):
    print(json.dumps(value, indent=2, sort_keys=True))


def manifest_errors(doc):
    errors = []
    if not isinstance(doc, dict):
        return ["manifest must be an object"]
    if doc.get("schema") != "kio-call-stack-perf-preparation-v1":
        errors.append("unsupported schema")
    if doc.get("state") != "reviewed-inputs-frozen":
        errors.append("state is not reviewed-inputs-frozen")

    def digest(value, label, pattern=HEX64):
        if not isinstance(value, str) or pattern.fullmatch(value) is None:
            errors.append(label + " must be a full lowercase hexadecimal identity")

    release = doc.get("release")
    if not isinstance(release, dict):
        release = {}
    if release.get("version") != "0.2.3":
        errors.append("release version must be 0.2.3")
    if not isinstance(release.get("published_url"), str) or not release["published_url"].startswith("https://"):
        errors.append("release published_url is missing")
    digest(release.get("commit"), "release.commit", HEX40)
    digest(release.get("publication_receipt_sha256"), "release.publication_receipt_sha256")
    if not isinstance(doc.get("experiment_branch_url"), str) or not doc["experiment_branch_url"].startswith("https://"):
        errors.append("experiment_branch_url is missing")
    arms = doc.get("arms")
    if not isinstance(arms, list) or len(arms) != 5 or any(not isinstance(a, dict) for a in arms):
        errors.append("arms must contain exactly five arm objects")
        arms = []
    if [a.get("id") for a in arms] != ARMS:
        errors.append("arms must be ordered B,T,C,S,P")
    for arm in arms:
        for field in ("commit", "tree"):
            digest(arm.get(field), str(arm.get("id")) + "." + field, HEX40)
        for field in ("compiler_sha256", "runner_receipt_sha256", "build_receipt_sha256"):
            digest(arm.get(field), str(arm.get("id")) + "." + field)
    commits = [a.get("commit") for a in arms if isinstance(a.get("commit"), str)]
    if len(commits) == 5 and len(set(commits)) != 5:
        errors.append("five distinct source commits are required")
    if arms and arms[0].get("commit") != release.get("commit"):
        errors.append("baseline commit differs from the release")
    for field in ("toolchain_receipt_sha256", "environment_receipt_sha256", "protocol_receipt_sha256", "review_receipt_sha256"):
        digest(doc.get(field), field)
    workloads = doc.get("workloads")
    if not isinstance(workloads, list) or not workloads:
        errors.append("at least one frozen workload is required")
        workloads = []
    ids = []
    for index, workload in enumerate(workloads):
        label = "workloads[" + str(index) + "]"
        if not isinstance(workload, dict):
            errors.append(label + " must be an object")
            continue
        identifier = workload.get("id")
        if not isinstance(identifier, str) or re.fullmatch(r"[a-z0-9][a-z0-9_-]*", identifier) is None:
            errors.append(label + ".id must be a portable identifier")
        else:
            ids.append(identifier)
        for field in ("source_sha256", "input_sha256", "oracle_sha256", "host_receipt_sha256"):
            digest(workload.get(field), label + "." + field)
    if len(ids) != len(set(ids)):
        errors.append("workload identifiers must be unique")
    return errors


def schedule(seed):
    # Odd-order Williams design: cyclic rows plus their reverses balance
    # positions and every ordered adjacent-arm pair over ten independent blocks.
    rng = random.Random(seed)
    labels = ARMS.copy()
    rng.shuffle(labels)
    pattern = [0, 1, 4, 2, 3]
    forward = [[labels[(x + shift) % 5] for x in pattern] for shift in range(5)]
    rng.shuffle(forward)
    reverse = [list(reversed(row)) for row in forward]
    rng.shuffle(reverse)
    return forward + reverse


def git_read(repo, *args):
    result = subprocess.run(["git", "--no-optional-locks", "--no-replace-objects", "-c", "core.fsmonitor=false", "-C", str(repo), *args], check=True, capture_output=True, text=True)
    return result.stdout.strip()


def source_identity(repo, commit):
    if HEX40.fullmatch(commit) is None:
        raise ValueError("pass a full 40-character commit, not a moving ref")
    resolved = git_read(repo, "rev-parse", "--verify", commit + "^{commit}")
    if resolved != commit:
        raise ValueError("commit identity did not resolve exactly")
    return {
        "commit": resolved,
        "tree": git_read(repo, "rev-parse", commit + "^{tree}"),
        "parents": git_read(repo, "show", "-s", "--format=%P", commit).split(),
        "checkout_head": git_read(repo, "rev-parse", "HEAD"),
        "checkout_status": git_read(repo, "status", "--porcelain=v1", "--untracked-files=normal"),
        "limit": "Git object identity only; this does not authenticate binaries or release publication",
    }


def file_inventory(root, names):
    root = pathlib.Path(root).resolve(strict=True)
    normalized = [pathlib.PurePath(name).as_posix() for name in names]
    if len(normalized) != len(set(normalized)):
        raise ValueError("duplicate file paths")
    rows = []
    for name in sorted(names):
        relative = pathlib.PurePath(name)
        if relative.is_absolute() or ".." in relative.parts or not relative.parts:
            raise ValueError("each file must be a relative path below the supplied root")
        candidate = root.joinpath(relative)
        # Reject every symlink component, not just links escaping the root.
        cursor = root
        for part in relative.parts:
            cursor = cursor / part
            if cursor.is_symlink():
                raise ValueError("symlink input: " + name)
        resolved = candidate.resolve(strict=True)
        if not resolved.is_relative_to(root) or not resolved.is_file():
            raise ValueError("not a regular file below root: " + name)
        before = resolved.stat()
        digest = hashlib.sha256()
        size = 0
        with resolved.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
                size += len(block)
        after = resolved.stat()
        if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns) != (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns):
            raise ValueError("input changed during hashing: " + name)
        rows.append({"path": relative.as_posix(), "bytes": size, "mode": oct(after.st_mode & 0o777), "sha256": digest.hexdigest()})
    encoded = json.dumps(rows, sort_keys=True, separators=(",", ":")).encode()
    return {"files": rows, "inventory_sha256": hashlib.sha256(encoded).hexdigest()}


def selfcheck():
    if not __debug__:
        raise ValueError("selfcheck requires assertions; unset PYTHONOPTIMIZE and omit -O")
    for seed in (0, 1, 20261015, 4294967295):
        rows = schedule(seed)
        assert rows == schedule(seed)
        assert len(rows) == 10 and all(sorted(row) == sorted(ARMS) for row in rows)
        for position in range(5):
            assert collections.Counter(row[position] for row in rows) == collections.Counter({arm: 2 for arm in ARMS})
        edges = collections.Counter((row[i], row[i + 1]) for row in rows for i in range(4))
        assert len(edges) == 20 and set(edges.values()) == {2}
    assert manifest_errors(None) and manifest_errors({})
    fixture = {
        "schema": "kio-call-stack-perf-preparation-v1", "state": "reviewed-inputs-frozen",
        "release": {"version": "0.2.3", "published_url": "https://example.invalid/release", "commit": "1" * 40, "publication_receipt_sha256": "a" * 64},
        "experiment_branch_url": "https://example.invalid/experiment",
        "arms": [{"id": arm, "commit": str(i + 1) * 40, "tree": "b" * 40, "compiler_sha256": "c" * 64, "runner_receipt_sha256": "d" * 64, "build_receipt_sha256": "e" * 64} for i, arm in enumerate(ARMS)],
        **{field: "a" * 64 for field in ("toolchain_receipt_sha256", "environment_receipt_sha256", "protocol_receipt_sha256", "review_receipt_sha256")},
        "workloads": [{"id": "mock", **{field: "f" * 64 for field in ("source_sha256", "input_sha256", "oracle_sha256", "host_receipt_sha256")}}],
    }
    assert manifest_errors(fixture) == []
    fixture["arms"][0]["commit"] = "5" * 40
    errors = manifest_errors(fixture)
    assert "five distinct source commits are required" in errors
    assert "baseline commit differs from the release" in errors
    fixture["arms"] = "bad"
    assert manifest_errors(fixture)
    emit({"status": "offline selfchecks passed", "measured_workloads": 0})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("selfcheck")
    sample = sub.add_parser("schedule")
    sample.add_argument("--seed", type=int, required=True)
    check = sub.add_parser("check-manifest")
    check.add_argument("manifest", type=pathlib.Path)
    identity = sub.add_parser("source-identity")
    identity.add_argument("repo", type=pathlib.Path)
    identity.add_argument("commit")
    inventory = sub.add_parser("hash-files")
    inventory.add_argument("root", type=pathlib.Path)
    inventory.add_argument("files", nargs="+")
    args = parser.parse_args()
    if args.command == "selfcheck":
        selfcheck()
    elif args.command == "schedule":
        if not 0 <= args.seed <= 4294967295:
            parser.error("seed must be between 0 and 4294967295")
        emit({"schema": "kio-perf-schedule-v1", "seed": args.seed, "blocks": schedule(args.seed), "scope": "one workload/backend/profile/cache-state cell; no commands executed"})
    elif args.command == "check-manifest":
        errors = manifest_errors(json.loads(args.manifest.read_text(encoding="utf-8")))
        emit({"complete": not errors, "errors": errors, "limit": "advisory completeness only, not execution authority"})
        return 1 if errors else 0
    elif args.command == "source-identity":
        emit(source_identity(args.repo, args.commit))
    elif args.command == "hash-files":
        emit(file_inventory(args.root, args.files))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as exc:
        print("preparation error: " + str(exc), file=sys.stderr)
        sys.exit(2)
