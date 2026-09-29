# lru_cache_trace

`lru_cache_trace` simulates a fixed sequence of cache `put` and `get`
operations over a three-slot least-recently-used cache. The cache state is kept
in mutable host arrays: keys, values, access ages, stats, and the replacement
victim log.

The case does not read stdin. `run.args` selects the `testapi-array` runner
protocol, whose host supplies strings, `I32` arithmetic, booleans, printing,
text formatting helpers, a recursive loop driver, and mutable arrays.

Stdout is a compact trace of every operation, followed by aggregate hit/miss
bookkeeping, the replacement log, and the final slot table. The slot table shows
the physical array slot, key, value, and last-access clock.

What this adds to the corpus: this is a medium data-structure-heavy workflow
that stresses opaque mutable arrays, array-backed bookkeeping, recursive scans
over fixed capacity, replacement selection, and cross-backend emission of a
stateful host type under the namespaced `testapi-array` protocol.
