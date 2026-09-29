# example-1

The reference contributed **library** case: a small Kio module with an
exported host function and two `equiv` laws about its locally defined
`scope!` elaborator's reduction, and no `main`. Its `run.test-only` marker
tells the harness to run `kio test` (which discharges the `equiv` laws) and
`kio build`, but never invoke a runner.

It is maintainer-owned (the `example-1` name), and — like `example-0` for
the `run.args` path — it keeps the `run.test-only` library path exercised
on every CI pass and doubles as the template a contributor copies for a
library.
