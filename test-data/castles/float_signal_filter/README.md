# float_signal_filter

`float_signal_filter` runs a fixed five-sample sensor stream through a
calibration stage, a weighted smoothing stage, and a residual-based scoring
stage. The data is embedded in the Kio modules so the report stays stable and
does not depend on host input.

There is no `input.stdin`. The runner protocol is `testapi-float`: role types
`String` and `F64` live at the `testapi` root, `print` is declared under
`testapi/io`, `f64_to_string` under `testapi/fmt`, and the F64 arithmetic
helpers under `testapi/arith`.

Stdout is a compact signal-processing report. It prints the raw stream, the
calibrated stream, the smoothed stream, three derived scores — level,
roughness, and trend — and a final signal score.

What this adds to the corpus: this is a compact-to-medium no-stdin
signal-processing castle centered on F64 role values and the `testapi-float`
host surface. It stresses multi-module float emission, role-typed decimal
literals, cross-module product threading, stable float stringification, and a
real pipeline shape rather than a trivial arithmetic demo.
