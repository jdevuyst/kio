# Ordered tree edit distance

This program compares small rooted, ordered trees whose nodes carry integer
labels. Insertion, deletion, and renaming each cost one. Deleting a node
promotes its ordered children into the deleted node's position. Inserting a
node is the inverse operation: it adopts a consecutive group of siblings,
possibly empty. Renaming changes only the node's label. Intermediate results
may be forests, including when a root is inserted or deleted.

The fixtures are built in `workdir/fixtures.kio` using open, leaf, and close
operations. There is no stdin or random seed. A tree has preorder label and
exclusive subtree-end arrays; those boundaries retain ancestry and sibling
order. The rendering `1(2(3,4),5)` means root 1 has children 2 and 5, and node 2
has children 3 and 4.

The memoized dynamic program compares two forest intervals. At their first
roots it considers deleting the source node, inserting the target node, or
pairing the roots with an optional rename. Deletion advances one node while
retaining its children in the forest. Pairing splits the problem into child
forests and remaining sibling forests. Empty-forest costs count every node.
The four interval endpoints index the memo table; the largest fixture needs
1,296 slots per table. Ties prefer pairing, then deletion, then insertion.

Traceback produces a node mapping. Stdout prints the actual source and target
trees, distance, mapped preorder positions, and delete/insert/rename/keep
counts. A separate certificate checks that the mapping is injective and
preserves ancestry in both directions: one mapped source node is an ancestor
of another if and only if their targets have that relation. Unrelated mapped
nodes retain their left-to-right order. The certificate derives operation
counts from unmapped nodes and unequal mapped labels, then compares their
total with the dynamic program's result.

The fixture witnesses are:

| Comparison | Distance | Reason |
| --- | --- | --- |
| `1(2(3,4),5)` to `1(3,4,5)` | 1 | Delete internal node 2 and promote children 3 and 4. |
| The reverse comparison | 1 | Insert node 2 around consecutive children 3 and 4. |
| `1(2,3)` to `1(3,2)` | 2 | Rename both children; a single edit cannot exchange their order. |
| `1(2(3))` to `1(2,3)` | 2 | Delete node 3 and insert it as a sibling of 2. The preorder labels agree, but ancestry differs. |
| Root 1 renamed to 6 in the branching tree | 1 | One root rename. |
| Internal node 2 renamed to 6 | 1 | One internal rename, retaining both descendants. |
| The branching tree compared with itself | 0 | Every node maps to itself. |

An independent exhaustive enumeration of bounded node mappings establishes
these expected distances: enumerate equally sized source and target subsets,
pair them in preorder, retain only mappings satisfying ancestry equivalence
and left-to-right order, and minimize the number of unmapped nodes plus
unequal mapped labels. Deleting unmapped source nodes, renaming retained
nodes, and inserting unmapped target nodes realizes that cost. The seven
fixtures examine 1,048 candidate mappings in total. The runtime also rejects
three deliberately invalid certificates: flattening an ancestor relationship,
introducing an ancestor relationship between former siblings, and reversing
siblings. The first two separately exercise both directions of ancestry
preservation.

The package uses the exact `testapi-array` host protocol and a materialized
dependency on the shared elaborator library. Iteration and recursive forest
decomposition use `rec(loop)`, including continuations for nested subproblems.

What this adds to the corpus: ordered tree/forest dynamic programming with
node deletion that promotes children, interval-based structural decomposition,
memoized costs, traceback, and independent mapping certificates. The equal
preorder/different ancestry fixture makes tree structure observable.
