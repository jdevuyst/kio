# grid_firebreak_search

This castle models a fixed 6 by 5 hazard grid. Each cell has an ignition risk,
a deployment cost, and optional barrier or candidate-firebreak status. The
program builds mutable host arrays for those grids, collects high-risk frontier
cells, scans each frontier cell's four neighbors, scores candidate firebreak
cells by exposed neighboring risk and cost, then greedily selects a plan within
a small budget.

The castle does not read `input.stdin`; all fixture data is encoded as fixed
grid facts in the Kio modules. `run.args` selects the `testapi-array` runner
protocol. Standard output is a compact summary of the grid size, frontier size,
candidate count, chosen cell indexes, budget use, risk exposure blocked, and
final score.

## What this adds to the corpus

This is a medium graph/search and planning case over a hazard grid. It combines
mutable host arrays, labeled domain records, a frontier-like work list, and
`rec(loop)` neighbor scans without being a union-find or minesweeper variant.
The main compiler stress is cross-module array-heavy state threading through
recursive search passes and testapi-conformed host capability imports.
