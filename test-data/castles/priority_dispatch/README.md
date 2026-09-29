# priority_dispatch

`priority_dispatch` models a small operations scheduler. A fixed backlog of
five jobs is dispatched across five capacity windows. Each job carries a
priority, deadline, CPU need, IO need, and duration. At each tick, the planner
filters out finished jobs and jobs whose resource needs do not fit the current
window, scores the remaining jobs by priority and deadline pressure, dispatches
the best candidate, and prints a compact trace.

This castle does not read stdin. The fixture is entirely in the Kio source so
the stdout trace is deterministic. `run.args` selects the documented
`testapi-arith-collection` runner protocol, and the package declares that
full environment exactly: role types at `testapi`, printing under
`testapi/io`, formatting under `testapi/fmt`, string concatenation under
`testapi/text`, i32 arithmetic and comparisons under `testapi/arith`, and
`loop` under `testapi/iter`.

Stdout starts with the trace header, then one line per capacity window, then a
summary. Each trace line names the tick, available CPU/IO capacity, chosen job,
computed score, deadline, duration, and the number of pending jobs blocked by
resource constraints in that window. The summary reports total dispatches,
accumulated blocked pending slots, consumed CPU/IO, selected duration, selected
deadline slack, and whether all jobs completed.

What this adds to the corpus: this is a scheduler/planner-style simulation
with labeled domain state, a recursive loop over a fixed planning horizon,
deadline-sensitive scoring, and resource-fit filtering. It is distinct from a
shift roster planner because it does not assign people to slots, and distinct
from a gate state machine because it is driven by priority ranking over a
backlog rather than command replay or transition-state validation.
