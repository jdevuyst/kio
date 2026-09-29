# interval_merge_planner

This castle models a no-stdin maintenance-window planner. The fixture is fixed
in source: six named windows are already sorted by start minute. The program
merges windows that overlap or touch, counts overlap conflicts separately from
touching joins, computes covered maintenance minutes and idle gaps between
merged segments, and prints a readable report.

There is no `input.stdin`; all input data is in
`workdir/interval_merge_planner/planner.kio`.

Stdout contains the raw fixture, the merged segments, idle gaps, detected
conflicts, and aggregate totals. Times are printed as minutes after midnight.

What this adds to the corpus: a no-stdin scheduler/planner castle over
record-shaped interval data. It stresses label records, row access, qualified
imports, branch-heavy arithmetic, the `testapi-arith-collection` host
protocol, and cross-backend emission of composed numeric and string workflows
without relying on raw intrinsic spellings.
