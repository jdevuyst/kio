# shift_roster_planner

`shift_roster_planner` scores a fixed four-day shift roster. The roster has
day/slot/worker assignments, worker skill checks, same-day double-booking
checks, night-to-morning rest checks, preference penalties, and a small fairness
penalty for uneven worker load.

There is no `input.stdin`; the checked-in fixture is the roster encoded in
the Kio modules. The runner protocol is `testapi-arith-collection`, and the
package declares that full environment, including unused tier functions.

Stdout prints a readable report: the roster rows, each worker's assigned load,
the constraint totals, the final score, and a boolean health line. The numbers
are intended to make the output understandable without inspecting the source.

What this adds to the corpus: this is a medium scheduler/planner castle. It
adds constraint scoring over fixed scenario data, pairwise conflict walks,
worker-load aggregation, label-heavy records, multi-module composition, and
`rec(loop)` traversals under the `testapi-arith-collection` host surface. It is
not a parser, graph walk, symbolic evaluator, array DP case, or state-machine
replay already represented by the existing castles.
