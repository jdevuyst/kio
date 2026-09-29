# Maximum flow and cut planner

This planner routes integer capacity through directed networks. Each original
link owns a separate reverse residual edge, so parallel links and an actual
link in the opposite direction retain distinct identities. A depth-first
search records predecessor edges, reconstructs an augmenting path, finds its
bottleneck, and updates both residual directions.

The fixtures are embedded in `flow/fixtures.kio`; no stdin is read:

- The four-node rerouting network first sends one unit along `0->1->2->3`.
  Its second path must cancel `1->2` and use `0->2->1->3`, reaching flow two.
- Parallel `0->1` links offer five units, but the shared `1->2` bottleneck
  admits four. A direct `0->2` link adds one, giving flow five. The separate
  `1->0` link exercises antiparallel edge identity.
- The disconnected network has a reachable cycle and a zero-capacity bridge
  into the sink's component. Its maximum flow and cut capacity are zero.

Stdout lists each augmenting path, amount, cancellation count, and final
per-link flow/capacity. A separate breadth-first traversal finds the residual
source side. The certificate module recomputes flow from original capacities,
checks capacity bounds and paired residual totals, accumulates every vertex's
net balance, and verifies that the cut is closed and equals the claimed flow.
The capacity bounds of the displayed cuts independently bound the optima.

Cloned witnesses deliberately violate a capacity bound, reverse-pair total,
vertex conservation, and reported flow value. The transcript checks rejection
of each defect and verifies that the original witness remains valid. The
cancellation witness also pins the two paths and the cancelled link's final
zero flow.

What this adds to the corpus: a mutable residual graph with reversible
decisions, predecessor reconstruction, independent graph traversal, and
primal/dual flow-and-cut certificates. Modules separate network construction,
search, augmentation, certificate checking, fixtures, and reporting. Imported
`if!`, `match!`, and `widen_sum!` compose the control flow through the shared
elaborator dependency; execution uses the exact `testapi-array` host protocol.
