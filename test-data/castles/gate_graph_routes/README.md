# gate_graph_routes

`gate_graph_routes` models a small fixed route planner over named gates A
through G. The program runs a deterministic relaxation walk across a schedule of
candidate directed edges. One edge is deliberately blocked, and gate G has no
incoming route, so the final summary reports route progress plus the
blocked-edge and unreachability facts.

The castle has no `input.stdin`; all route data is embedded in the Kio
modules. `expected.stdout` is a one-line summary: selected route, final cost,
edge visits, successful relaxations, how many times the blocked edge was seen,
and whether gate G remained unreachable.

What this adds to the corpus: this is a medium graph/search castle. It stresses
multi-module composition, label-heavy state records, recursive `rec(loop)`
control flow, nested deterministic branching over edge candidates, `I32`
arithmetic/comparison, boolean formatting, and compact host-output rendering
under the `testapi-arith-collection` protocol.
