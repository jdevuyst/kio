# Sensor Quorum Array

This castle builds mutable arrays for sensor readings, thresholds, and pass
flags, then marks each sensor and reports whether the quorum passed. The data is
embedded in source; there is no stdin fixture and no dependency.

Stdout prints the number of sensors, the pass count, each sensor row, and a
clone/swap check from a reordered copy of the flag array.

What this adds to the corpus: an array-heavy workflow using the `testapi-array`
protocol. It exercises mutable array construction, generic filled arrays,
recursive array traversal, array updates, cloning, swapping, boolean rendering,
and formatted numeric reporting.
