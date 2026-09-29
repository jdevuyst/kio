# E-graph equality saturation

This program retains equivalent integer expressions in an e-graph, rebuilds
congruence after class unions, and extracts a least-cost expression from the
available alternatives. Fixed expression fixtures use constants, the variables
`x` and `y`, addition, and multiplication. There is no stdin.

Nodes have labeled shapes and child class IDs. Each inserted node starts with a
class; union/find chooses a canonical class representative. Interning compares
operators and canonical children. Rebuilding repeatedly merges nodes whose
keys became equal after earlier unions, until a complete pass makes no merge.
The original expression nodes remain available alongside the added alternatives.

The equality rules are deliberately small: addition with zero, multiplication
with zero or one (on either side), constant addition and multiplication, and
`a*b + a*c = a*(b+c)` for a shared left factor. A rule searches class members,
not just a single representative expression. Each round visits the nodes present
at its start; added nodes become rewrite subjects in the next round. After each
round the graph rebuilds congruence. `saturated` means an entire round made no
node insertion or class union; `exhausted` means the round budget ended while
changes were still occurring. This is saturation for these rules and the current
graph, not completeness for arbitrary arithmetic equalities.

Extraction assigns unit cost to each expression-tree node. Repeated relaxation
chooses the cheapest node in each class using the current costs of its child
classes. Positive operator costs keep a cyclic alternative, such as `x+0` in
the class of `x`, from beating a finite leaf. Rendering and evaluation follow
the actual selected nodes. The fixtures keep all arithmetic small; they are
examples over these bounded expressions rather than an unrestricted optimizer
or an overflow-analysis tool.

The report shows five fixtures:

- A zero-addition merge makes the child classes of two initially distinct
  parents equal. The parents remain distinct until rebuilding performs a merge.
- Factoring `x*2+x*3` creates the real alternative `x*(2+3)`, reducing cost from
  seven to five. A one-round budget reports exhaustion at that point. Resuming
  folds `2+3`, extracts `x*5` at cost three, and reaches a fixed point. Rechecking
  the fixed point leaves the graph unchanged.
- Zero and one identities create class cycles while extraction still chooses
  `x` at cost one.
- Constant addition followed by multiplication extracts `20` at cost one.
- `x+y` already has no applicable equality and saturates without a graph change.

`alternatives` counts nodes in the result class, while `nodes` and `classes`
show the retained graph size. `values` evaluates the extracted expression at
`(x,y) = (-2,5), (0,1), (3,-1)`. `source-agrees` compares those values with a
separate traversal of the unchanged original expression nodes. The printed
costs count tree nodes, including repeated occurrences of a shared child.

The package uses the exact `testapi-array` runner protocol, directly depends on
`elab`, and configures all eight host targets. Its array walks, rewriting,
rebuilding, extraction, and expression traversals use ordinary `rec(loop)`.

What this adds to the corpus: equivalence-class mutation, deferred congruence
repair, member-based equality matching, explicit saturation budgets, and cost
relaxation over a graph that retains competing expressions and class cycles.
