# float_pid_controller

`float_pid_controller` models a fixed three-sample PID-like control pass over
F64 sensor readings. The package calibrates each raw reading, computes the
setpoint error, rolls the accumulated error forward, computes a derivative from
the previous error, and combines proportional, integral, and derivative terms
into a control command.

The castle has no `input.stdin`; the readings, calibration constants, PID
gains, and starting controller memory are fixed in the package source so every
backend runs the same scenario. `run.args` selects the `testapi-float` runner
protocol.

`expected.stdout` is a compact report. It prints the controller title, then the
calibrated value, error, accumulated integral, derivative, and control command
for each of the three controller updates.

What this adds to the corpus: this is a medium numeric simulation centered on
floating-point control state. It uses the `testapi-float` host surface, row
labels for controller records, and cross-module composition of calibration,
error-memory update, term calculation, and reporting. The focus is PID state
and error terms rather than a simple signal filter.
