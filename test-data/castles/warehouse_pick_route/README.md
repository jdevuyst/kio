# warehouse_pick_route

This castle models a small warehouse pick-route planner over a fixed aisle
graph. The package builds an adjacency matrix, marks one cross-aisle link as
blocked, computes shortest travel distances with mutable arrays, then greedily
chooses the next unpicked order line by nearest reachable location. The final
leg returns the picker to the packing station.

The graph and order fixture are compiled into the Kio source. There is no
`input.stdin`; `run.args` selects the `testapi-array` runner protocol.

Stdout reports the fixture size, the blocked-aisle flag, the chosen route, the
pick summary, and the total travel score. The route is readable as warehouse
locations rather than node numbers.

What this adds to the corpus: a graph/search and route-planning program that
uses the testapi-conformed mutable array host surface as the core algorithmic
state, including an adjacency matrix, settled-distance arrays, pick-state
arrays, and route accumulation. It stresses generic host arrays, namespaced
host declarations, `rec(loop)` lowering, multi-backend emission, and realistic
composition without stdin.
