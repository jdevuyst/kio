import { createBench, type BenchCallable, type BenchHost } from "../workload/out/ts/bench.js";

// Declaration-only consumer: typecheck this function, do not execute it.
export function checkSurface(lift: (body: (value: number) => number) => BenchCallable<number, number>): number {
  const host: BenchHost = { bench: { step: (value, label) => value + label + 1 } };
  const pkg = createBench(host);
  const root = pkg.bench.seed();
  const tail = pkg.bench.tail(root, 1);
  const pending = pkg.bench.pending(tail, 2);
  const callback: BenchCallable<number, number> = pkg.bench.callbackSeed();
  const forwarded = pkg.bench.callbackPending(pkg.bench.callbackTail(callback, 1), 2);
  const native = lift((value) => host.bench.step(value, 0));
  return pkg.bench.direct(7) + pkg.bench.invoke(pending, 7) + forwarded(7) + pkg.bench.callNative(native, 7);
}
