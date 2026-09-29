# calendar_blackout_planner

This castle scores three release windows against support load, priority, and a
blackout flag. The data is embedded in Kio source; there is no stdin fixture and
no dependency.

Stdout lists the score for each window, the chosen release window, and the
release/frozen totals. The scoring module treats frozen windows as unavailable
and otherwise subtracts the support-load penalty from the rollout priority.

What this adds to the corpus: a scheduler/planner-shaped castle using
record-labeled domain data and the `testapi-arith-collection` host surface. It
exercises label access, integer scoring, boolean branch control, and report
composition without arrays or stdin.
