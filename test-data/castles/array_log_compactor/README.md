# array_log_compactor

`array_log_compactor` models a mutable event-log cleanup pass. The program builds
an in-memory log of keyed updates and delete markers, scans the array for records
that have a later event for the same key, and rewrites the latest live records
into the front of the same working array.

The castle has no `input.stdin`; the event fixture is checked into the Kio source
so the stdout transcript is stable. Stdout reports the original log length, the
number of live records kept, the number of records removed as superseded, the
number of delete markers removed, a checksum over the compacted live prefix, and
the compacted records in prefix order.

What this adds to the corpus: this is a medium, data-structure-heavy mutable
array workflow, not a dynamic-programming table. It stresses host arrays holding
labeled record products, cloned working state, in-place `array_set` compaction,
recursive scans over array indices, string-key equality, and formatted summary
output through the `testapi-array` runner protocol.
