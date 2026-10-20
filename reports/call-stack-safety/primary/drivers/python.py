"""Public Python host for the archived scalar workload."""

import argparse
import importlib.util
import json
import pathlib
import time
import types

MODES = ("direct", "native", "host-loop", "tail", "pending", "callback-tail", "callback-pending")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifact", type=pathlib.Path)
    parser.add_argument("arm", choices=("B", "T", "C", "S", "P"))
    parser.add_argument("mode", choices=MODES)
    parser.add_argument("depth", type=int)
    parser.add_argument("repeat", type=int)
    parser.add_argument("seed", type=int)
    parser.add_argument("warmup", type=int)
    parser.add_argument("purpose", choices=("time", "witness"))
    args = parser.parse_args()
    if not (0 <= args.depth <= 10000 and 1 <= args.repeat <= 1000000
            and 0 <= args.seed < 65536 and 0 <= args.warmup <= 1000000):
        parser.error("bounds: depth 0..10000, repeat 1..1000000, seed 0..65535, warmup 0..1000000")
    if args.mode in ("direct", "native") and args.depth != 0:
        parser.error("direct/native require depth 0")
    if args.purpose == "witness" and (args.depth > 8 or args.repeat != 1 or args.warmup != 0):
        parser.error("witness requires depth <=8, repeat 1, warmup 0")
    spec = importlib.util.spec_from_file_location("bench_generated", args.artifact)
    if spec is None or spec.loader is None:
        raise ValueError("cannot load generated public module")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    trace = []

    def step(value, label):
        if args.purpose == "witness":
            trace.append(label)
        return value + label + 1

    start = time.perf_counter_ns()
    pkg = mod.create_bench(types.SimpleNamespace(
        KioHostIn_bench_I32=lambda value: value,
        KioHostOut_bench_I32=lambda value: value,
        bench=types.SimpleNamespace(step=step),
    ))
    package_ns = time.perf_counter_ns() - start
    start = time.perf_counter_ns()
    retained = []
    if args.mode in ("tail", "pending"):
        retained.append(pkg.bench.seed())
        builder = getattr(pkg.bench, args.mode)
        for label in range(1, args.depth + 1):
            retained.append(builder(retained[-1], label))
        invoke = lambda value: pkg.bench.invoke(retained[-1], value)
    elif args.mode.startswith("callback-"):
        retained.append(pkg.bench.callbackSeed())
        builder = pkg.bench.callbackTail if args.mode == "callback-tail" else pkg.bench.callbackPending
        for label in range(1, args.depth + 1):
            retained.append(builder(retained[-1], label))
        invoke = lambda value: retained[-1](value)
    elif args.mode == "native":
        callback = mod.kio_lift(lambda value: step(value, 0))
        invoke = lambda value: pkg.bench.callNative(callback, value)
    elif args.mode == "direct":
        invoke = pkg.bench.direct
    else:
        def invoke(value):
            for label in range(args.depth, -1, -1):
                value = step(value, label)
            return value
    construct_ns = time.perf_counter_ns() - start
    if trace:
        raise ValueError("constructing closures unexpectedly invoked a host body")
    delta = (args.depth + 1) * (args.depth + 2) // 2
    expected = sum((args.seed + index * 17) % 65536 + delta for index in range(args.repeat))
    start = time.perf_counter_ns()
    for index in range(args.warmup):
        value = (args.seed + index * 17) % 65536
        if invoke(value) != value + delta:
            raise ValueError("warmup result mismatch")
    warmup_ns = time.perf_counter_ns() - start
    start = time.perf_counter_ns()
    checksum = 0
    for index in range(args.repeat):
        checksum += invoke((args.seed + index * 17) % 65536)
    invoke_ns = time.perf_counter_ns() - start
    if checksum != expected:
        raise ValueError("measured checksum mismatch")
    if args.purpose == "witness":
        wanted = list(range(args.depth + 1)) if args.mode in ("pending", "callback-pending") else list(range(args.depth, -1, -1))
        if trace != wanted:
            raise ValueError("host effect order mismatch")
    start = time.perf_counter_ns()
    invoke = None
    callback = None
    builder = None
    while retained:
        retained.pop()
    pkg = None
    release_roots_ns = time.perf_counter_ns() - start
    print(json.dumps({
        "schema": "kio-primary-driver-v1", "backend": "python", "arm": args.arm,
        "mode": args.mode, "depth": args.depth, "repeat": args.repeat,
        "seed": args.seed, "warmup": args.warmup, "purpose": args.purpose,
        "checksum": checksum, "expected": expected, "trace": trace,
        "package_ns": package_ns, "construct_ns": construct_ns,
        "warmup_ns": warmup_ns, "invoke_ns": invoke_ns,
        "release_roots_ns": release_roots_ns,
        "total_phases_ns": package_ns + construct_ns + warmup_ns + invoke_ns + release_roots_ns,
        "release_limit": "roots released in reverse construction order; no forced GC or live-heap claim",
    }, sort_keys=True))


if __name__ == "__main__":
    main()
