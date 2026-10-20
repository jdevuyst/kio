// Public host driver. The launcher supplies benchFactory, benchClockNs and
// benchConfigJson; it never reads generated private fields or rewrites output.
(() => {
  "use strict";
  const c = JSON.parse(benchConfigJson);
  const keys = ["arm", "depth", "mode", "purpose", "repeat", "seed", "warmup"];
  if (c === null || typeof c !== "object" || Array.isArray(c) ||
      JSON.stringify(Object.keys(c).sort()) !== JSON.stringify(keys)) {
    throw Error("configuration must contain exactly the seven declared fields");
  }
  const modes = ["direct", "native", "host-loop", "tail", "pending", "callback-tail", "callback-pending"];
  if (!["B", "T", "C", "S", "P"].includes(c.arm) || !modes.includes(c.mode) ||
      !["time", "witness"].includes(c.purpose) ||
      !Number.isInteger(c.depth) || c.depth < 0 || c.depth > 10000 ||
      !Number.isInteger(c.repeat) || c.repeat < 1 || c.repeat > 1000000 ||
      !Number.isInteger(c.seed) || c.seed < 0 || c.seed >= 65536 ||
      !Number.isInteger(c.warmup) || c.warmup < 0 || c.warmup > 1000000 ||
      (["direct", "native"].includes(c.mode) && c.depth !== 0) ||
      (c.purpose === "witness" && (c.depth > 8 || c.repeat !== 1 || c.warmup !== 0))) {
    throw Error("invalid bounded driver configuration");
  }
  const trace = [];
  const step = (value, label) => {
    if (c.purpose === "witness") trace.push(label);
    return value + label + 1;
  };
  let start = benchClockNs();
  let pkg = benchFactory({bench: {step}});
  const package_ns = benchClockNs() - start;
  start = benchClockNs();
  const retained = [];
  let invoke;
  let callback = null;
  if (c.mode === "tail" || c.mode === "pending") {
    retained.push(pkg.bench.seed());
    for (let label = 1; label <= c.depth; label++) {
      retained.push(pkg.bench[c.mode](retained[retained.length - 1], label));
    }
    invoke = value => pkg.bench.invoke(retained[retained.length - 1], value);
  } else if (c.mode.startsWith("callback-")) {
    retained.push(pkg.bench.callbackSeed());
    const builder = c.mode === "callback-tail" ? pkg.bench.callbackTail : pkg.bench.callbackPending;
    for (let label = 1; label <= c.depth; label++) retained.push(builder(retained[retained.length - 1], label));
    invoke = value => retained[retained.length - 1](value);
  } else if (c.mode === "native") {
    const body = value => step(value, 0);
    // Explicit revision adapter, never capability probing or fallback:
    // B/T accept native functions; C/S/P require the public lift helper.
    callback = ["B", "T"].includes(c.arm) ? body : benchFactory.lift(body);
    invoke = value => pkg.bench.callNative(callback, value);
  } else if (c.mode === "direct") {
    invoke = value => pkg.bench.direct(value);
  } else {
    invoke = value => {
      for (let label = c.depth; label >= 0; label--) value = step(value, label);
      return value;
    };
  }
  const construct_ns = benchClockNs() - start;
  if (trace.length) throw Error("construction invoked a host body");
  const delta = (c.depth + 1) * (c.depth + 2) / 2;
  let expected = 0;
  for (let i = 0; i < c.repeat; i++) expected += (c.seed + i * 17) % 65536 + delta;
  start = benchClockNs();
  for (let i = 0; i < c.warmup; i++) {
    const value = (c.seed + i * 17) % 65536;
    if (invoke(value) !== value + delta) throw Error("warmup result mismatch");
  }
  const warmup_ns = benchClockNs() - start;
  start = benchClockNs();
  let checksum = 0;
  for (let i = 0; i < c.repeat; i++) checksum += invoke((c.seed + i * 17) % 65536);
  const invoke_ns = benchClockNs() - start;
  if (checksum !== expected) throw Error("measured checksum mismatch");
  if (c.purpose === "witness") {
    const wanted = Array.from({length: c.depth + 1}, (_, i) =>
      ["pending", "callback-pending"].includes(c.mode) ? i : c.depth - i);
    if (JSON.stringify(trace) !== JSON.stringify(wanted)) throw Error("host effect order mismatch");
  }
  start = benchClockNs();
  invoke = null;
  callback = null;
  while (retained.length) retained.pop();
  pkg = null;
  const release_roots_ns = benchClockNs() - start;
  return JSON.stringify({
    schema: "kio-primary-driver-v1", backend: "quickjs",
    arm: c.arm, mode: c.mode, depth: c.depth, repeat: c.repeat,
    seed: c.seed, warmup: c.warmup, purpose: c.purpose,
    checksum, expected, trace, package_ns, construct_ns, warmup_ns, invoke_ns,
    release_roots_ns,
    total_phases_ns: package_ns + construct_ns + warmup_ns + invoke_ns + release_roots_ns,
    release_limit: "roots released in reverse construction order; no forced GC or live-heap claim"
  });
})();
