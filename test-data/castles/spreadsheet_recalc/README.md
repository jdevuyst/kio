# spreadsheet_recalc

An incremental recalculation engine for a small spreadsheet. The program
builds the dependency graph of a 4x4 sheet of formula cells, reports the
cells caught on a reference cycle, evaluates the sheet in topological
order, and then applies a short script of edits — recomputing, for each
one, only the cells the edit can actually reach.

The program reads no stdin: the sheet and the edit script are the fixture,
compiled into `sheet/model`.

## The sheet

Sixteen cells, addressed `A1`..`D4` (column letter, 1-based row). Each cell
holds a number, a formula, or nothing at all.

```text
  A1 = 5             B1 = 3              C1 = 8             D1 = SUM(A1:C1)
  A2 = 2             B2 = (A1 + B1)      C2 = (B2 * A2)     D2 = MAX(A1:C2)
  A3 = (D1 - D2)     B3 = IF(A3, C1, A2) C3 = (D3 + 1)      D3 = (C3 + 2)
  A4 = (C3 * 2)      B4 = SUM(A1:D1)     C4 = -             D4 = (B4 + C4)
```

`C4` is empty. Every formula that reads it treats the blank as zero — so
`D4` is `B4 + 0` — while `MAX` skips blanks rather than counting them as
candidates.

## The formula language

An expression (`sheet/formula`) is one of:

| Form | Meaning |
| --- | --- |
| `A1` | the value of that cell |
| `7` | a literal |
| `(e + e)`, `(e - e)`, `(e * e)` | arithmetic |
| `SUM(A1:C2)` | the sum over a rectangular range |
| `MAX(A1:C2)` | the largest value in a rectangular range |
| `IF(c, t, e)` | `t` when `c` is non-zero, `e` when it is zero |

The tree is a `labels`-generated sum grounded at a `newtype`, and every
pass over it — the precedent walk, the evaluator, the renderer — is one
exhaustive `match!`.

A cell's **precedents** are the cells its formula reads: the references in
the tree, plus every cell a range covers. Inverting that relation gives the
**dependents** map: for each cell, who reads it.

## The cycle

`C3` reads `D3` and `D3` reads `C3`. That is a genuine reference cycle, and
neither cell has a well-founded value: both evaluate to `#CYCLE`. The error
is an ordinary arm of the cell-value sum, not a crash — the run still exits
`0`.

The engine reports the cells that are *on* a cycle, which is not the same
as the cells that *have* the error. A cell is on a cycle exactly when it can
reach itself by following precedents, so the report names `C3` and `D3` but
not `A4` — `A4 = (C3 * 2)` reaches the cycle and never comes back, so it is
downstream of it. `A4` picks up `#CYCLE` by evaluating, the same way any
other error propagates through arithmetic.

Cells on a cycle are settled as `#CYCLE` before the ordering starts. That is
what lets Kahn's algorithm order everything else: the cycle is exactly what
would otherwise leave the ready queue empty with work remaining.

## The edit script

Four edits, one per section of the transcript:

1. **`A1 = 9`** — set a literal. The most widely read cell in the sheet.
2. **`D3 = (B3 + 4)`** — replace a formula. `D3` no longer names `C3`, which
   **breaks** the cycle: `C3`, `D3`, and `A4` become ordinary numbers.
3. **`B4` cleared** — clear a cell. `B4` goes blank and `D4` re-adds it as
   zero.
4. **`A3 = (B3 + 1)`** — replace a formula. `B3` already reads `A3`, so this
   **closes a new cycle** between them, and the three cells downstream of it
   inherit `#CYCLE`.

The graph is rebuilt after each edit, because an edit can change it: edits 2
and 4 change which cells cycle, not just which values they hold.

## What the output means

The opening section prints the sheet's dimensions, the cell definitions as
the user typed them, and the evaluated grid — values, `#CYCLE` where the
cycle reaches, `.` for an empty cell — followed by:

- `cycle:` the cells on a reference cycle,
- `topological order:` the order the remaining cells were evaluated in,
  every cell after all of its precedents,
- `ordered:` how many cells that order covers (16 minus the cycle).

Then one section per edit:

- `dirty:` the cells the edit can change — the edited cell plus everything
  that reads it, transitively — in address order,
- `recompute order:` those same cells in the order they were recomputed:
  dirty cells on a cycle first (they are settled, not computed), then the
  rest in the new topological order, so each one reads precedents that are
  already up to date,
- `recomputed:` / `skipped:` how many cells the edit touched and how many
  kept the value they already had. **This is the point of the castle**: a
  full pass would be 16 every time; edit 3 recomputes 2 and skips 14.
- `cycle:` the cells on a cycle *after* the edit,
- `grid:` the sheet as it now stands.

Every count is hand-checkable from the sheet above. Edit 1 dirties `A1` and
the eight cells that transitively read it; edit 2 dirties three; edit 3
dirties two; edit 4 dirties five.

## Modules

| Module | Role |
| --- | --- |
| `sheet/list` | the cons list every sequence is built from |
| `sheet/addr` | addresses, the 4x4 geometry, address sets |
| `sheet/table` | an address-keyed table, generic in its payload |
| `sheet/formula` | the expression tree, its precedents, its syntax |
| `sheet/cells` | cell content, cell values, and their eliminators |
| `sheet/graph` | precedents, dependents, in-degrees, cycles, Kahn |
| `sheet/eval` | evaluating one cell against the value store |
| `sheet/recalc` | the plan, the full pass, the dirty-set pass |
| `sheet/render` | the grid and the transcript |
| `sheet/model` | the fixture sheet and the edit script |

The host tier (`testapi-arith-collection`) has no arrays, no division, and
no string length. Sequences are therefore Kio lists, an address is a nominal
`(row & col)` pair rather than a packed integer that could never be taken
apart again, and the grid computes its own column widths from the values
instead of measuring the rendered text.

What this adds to the corpus: the corpus's first incremental-recomputation
engine — a formula dependency graph with topological ordering, genuine cycle
detection with error propagation, and dirty-set recalculation where the
recomputed-vs-skipped counts are the observable result. Where
`task_dependency_toposort` orders a fixed graph once and `gate_symbolic_eval`
folds a single expression tree, this castle keeps a graph, an expression
language, and a value store in step with each other across a script of edits
that rewrite the graph underneath it — exercising a generic address-keyed
table, a polymorphic cons list, an eight-arm expression sum matched by
`match!`, an error value that propagates through evaluation rather than
aborting it, and `rec(loop)` walks whose termination depends on a visited
set because the graph they traverse may cycle.
