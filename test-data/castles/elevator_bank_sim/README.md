# elevator_bank_sim

`elevator_bank_sim` models a fixed two-car elevator bank in a six-floor
building. It runs twelve deterministic ticks. Each tick adds scheduled hall
requests at the lobby, middle, or high floor, moves both cars one floor toward
their current dispatch target, serves at most one request per car, and tracks
waiting passengers, total served requests, accumulated wait, and a final score.

The case has no `input.stdin`. Its only execution fixture is `run.args`, which
selects `--protocol testapi-arith-collection`. Stdout is the full simulation
transcript: one line per tick with arrivals, car floors, direction booleans,
served count, remaining wait, and cumulative wait, followed by a summary line.

What this adds to the corpus: a medium game/state-machine simulation that uses
label-built product state and `rec(loop)` for an iterative tick driver. It
exercises the namespaced `testapi` arithmetic-collection protocol across
integer arithmetic, comparisons, boolean rendering, string concatenation, and
multi-backend emission without arrays or stdin.
