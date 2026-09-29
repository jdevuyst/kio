# sudoku_backtrack_solver

A 9x9 sudoku solver written as an explicit backtracking search: a
depth-first walk that places a digit, descends, and — when the descent
fails — retracts exactly what it placed and tries the next digit. Three
grids ship in the source. The program prints each one, the verdict, an
independent re-check of that verdict, and what the search cost.

The program reads no input; there is no `input.stdin`.

## The three puzzles

| # | name | givens | outcome |
| --- | --- | --- | --- |
| 1 | `gentle` | 30 | solved without ever retracting a digit |
| 2 | `hard` | 23 | solved, but only through real backtracking |
| 3 | `contradictory` | 24 | proven to have no completion at all |

The **gentle** grid is forced all the way down: every cell the heuristic
reaches has exactly one digit left open to it, so the search walks
straight to the answer and never uses its undo path.

The **hard** grid has one solution and no way to reach it by forcing
alone. The heuristic runs out of singleton cells early, the search has to
commit to a digit it cannot justify, and it discovers its mistake many
cells later. That is what the `backtracks` count measures.

The **contradictory** grid is the hard grid with a single extra clue: an
`8` in the seventh row, second column. That `8` duplicates no digit in its
row, in its column, or in its box, so every local check accepts it — and
yet the grid it produces admits no completion whatsoever. Nothing short of
search can say so, which is exactly the point: the solver has to explore,
fail everywhere, unwind every placement it made, and *then* report
`unsolvable`. It is not allowed to crash, and it is not allowed to spin.

## Constraint bookkeeping, and the undo discipline

A digit is legal in a cell when none of the cell's three units — its row,
its column, its 3x3 box — already holds that digit. Deciding that by
rescanning the board would cost 27 reads per probe, and the search probes
constantly, so the solver keeps the answer instead of recomputing it.

Three tables (`used_row`, `used_col`, `used_box` in `sudoku/model`) each
hold nine units by nine digits: slot `unit * 9 + (digit - 1)` is `1` when
that unit already contains that digit. This tier has no bit operators, so
the "bitmask" of digits used by a unit is spread across nine array slots
rather than packed into nine bits of one integer; the bookkeeping is the
same either way. A legality test is then three reads, and a candidate
count for a cell is nine of those.

The tables are only ever *maintained*, never rebuilt. They are seeded once
from the givens, and from then on:

- `place` writes the digit into the board and sets **exactly three** table
  slots;
- `unplace` clears the board slot and clears **exactly those same three**.

That symmetry is the whole discipline. `sudoku/constraints` writes both
halves through one shared `mark` helper precisely so they cannot drift
apart — an undo that cleared two slots out of three would leave a phantom
digit blocking a unit forever, and the search would go on to "prove"
things that are not true.

Because the trail is exact, a refuted grid comes back byte for byte: after
the `contradictory` run the board is once again the grid it started from,
and the program checks and prints that (`board restored to the givens`).
Three hundred placements went in and three hundred came back out.

## The search stack

There is no host recursion in this tier, so the depth-first walk cannot
lean on a call stack — it carries its own. `stack_cell` and `stack_digit`
are the frames: the cell chosen at each depth, and the digit currently
placed there. A frame is therefore also its own undo record, since
retracting it needs precisely that pair.

`sudoku/search` drives the whole thing as one `rec(loop)` alternating
between two phases:

- **descending** — ask the heuristic for a cell and open a frame on it;
- **advancing** — work on the top frame: first retract whatever it is
  holding, then try the next digit up. A frame with nothing left to try is
  popped, and the search advances into its parent.

So a failure walks back up the stack one frame at a time. Reaching an
empty stack while advancing means every possibility has been eliminated:
the grid is unsolvable.

## The heuristic

The next cell is the empty one with the fewest digits still open to it —
*minimum remaining values*. It is worth having twice over. It makes the
gentle grid free, because a cell with one candidate is not a guess at all.
And it makes failure cheap on the hard grids, because a cell with **zero**
candidates is a dead end, and MRV finds those first: the search learns its
last digit was wrong at the earliest possible moment instead of burrowing
deeper into a doomed subtree.

The scan stops early as soon as it sees a cell with one candidate or none,
since no later cell could beat it.

## Reading the output

Each puzzle prints its grid (`.` for an empty cell), the verdict, a
re-check, and four numbers.

The re-check is deliberately *not* the solver's own opinion.
`sudoku/verify` rebuilds row, column, and box occupancy from the finished
board alone and never looks at the search's tables, so a bug in the undo
trail cannot make a wrong grid look right — both would have to be wrong in
the same way. It reports whether every row, column, and box really holds
1-9 exactly once, and whether every original clue survived untouched.

| stat | meaning |
| --- | --- |
| `placements` | digits written into a cell by the search |
| `backtracks` | digits retracted again — every undo |
| `max depth` | the deepest the search stack ever got |
| `steps` | loop iterations spent |

On a solved grid, `placements - backtracks` is the number of digits left
standing at the end. On a refuted one the two are necessarily equal:
everything that went in came back out. `gentle` reports zero backtracks —
it never guessed. `hard` reports 161 — it guessed, and paid for it.

`steps` is capped by a budget (200000 iterations, printed in the header).
No shipped grid comes near it; it is there so a pathological puzzle would
stop with a `budget exhausted` verdict instead of hanging.

## What this adds to the corpus

The corpus's first backtracking search: an explicit search stack with an
undo trail, incremental constraint tables restored on backtrack, the
minimum-remaining-values heuristic, and a proven-unsolvable case. Where
the other array castles sweep a grid forward — scoring it, flooding it,
compacting it — this one has to *take moves back*, and its correctness
rests on the retraction being exactly as precise as the placement. The
unsolvable fixture is the part no forward-only castle can express: a
verdict that is only reachable by exhausting a search space, printed
alongside the proof that every placement made along the way was undone.
