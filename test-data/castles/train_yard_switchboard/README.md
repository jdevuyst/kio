# Train Yard Switchboard

This castle simulates a small train-yard switchboard with mutable arrays of
track occupancy. It swaps an arriving train into an empty track, writes a new
departure slot, and reports the final yard safety state. The data is embedded in
source; there is no stdin fixture and no dependency.

Stdout prints the before and after yard state, the final occupied count, and a
boolean safety result.

What this adds to the corpus: a game/state-machine-style array workflow using
the `testapi-array` protocol. It exercises mutable string arrays, cloning,
swapping, setting, recursive occupancy counting, string equality, and boolean
reporting.
