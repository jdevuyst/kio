# Task Dependency Toposort

This castle models a small release pipeline as a dependency graph and computes
a deterministic topological order. The fixture is compiled into the Kio source:
seven tasks have fixed integer IDs, names, effort scores, and dependency rules.

The program reads no stdin. It prints the task count, completed count, loop
rounds, ready and blocked counts at the stopping point, peak ready and blocked
counts seen during planning, a cycle-risk flag, an effort score, and the chosen
order. A clean plan completes all tasks with `cycle-risk: false`; if a scan ever
finds no ready task while work remains, the same planner would stop with the
remaining blocked count and `cycle-risk: true`.

What this adds to the corpus: a medium scheduler/planner castle using a
compiled graph fixture, record-shaped state threaded through `rec(loop)`, and
counting scans over task IDs. It stresses labels, row access, multi-module
imports, recursive loop lowering, i32 comparison/arithmetic, and the
testapi-conformed `arith-collection` host surface without relying on stdin,
arrays, or raw intrinsic spellings.
