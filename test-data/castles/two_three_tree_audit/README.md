# two_three_tree_audit

A 2-3 tree that checks its own balance after every operation it performs.

The tree holds integer keys with string payloads. A script of fourteen inserts
and eight deletes runs against it; after each one the tree is audited from
scratch — recomputed from the tree alone, trusting nothing the insert or delete
path claimed — and one transcript line reports what happened, how tall and how
large the tree now is, and whether it is still a legal 2-3 tree.

The program reads no input. `run.args` selects the `testapi-arith-collection`
protocol, whose host surface is integers, strings, booleans, and `loop` — no
division, no string inspection, no arrays. Everything else is Kio.

## The tree, and the invariant it exists for

A node is a leaf, a **2-node** (one key, two children), or a **3-node** (two
keys, three children). Keys are ordered as in any search tree: everything left
of a key is smaller, everything right is larger, and a 3-node's middle child
holds the keys between its two.

What makes it a 2-3 tree rather than a plain search tree is one extra promise:

> **every leaf sits at exactly the same depth.**

That is the whole point. It is what bounds the height, and so bounds every
lookup, and it is what the rebalancing below exists to preserve. It is also a
property of a *value at runtime* — the node type cannot carry it, and encoding
it in the type would make the audit vacuous. So the tree maintains it, and the
audit checks it.

## Insert: split, and promote

Insertion never grows a leaf into a node — that would push one leaf a level
deeper than its neighbours and break the invariant on the very first key.
Instead, a descent into a subtree answers with one of two things
(`tree/insert`):

- `fitted(n)` — the subtree took the key and kept its height.
- `split(a, k, v, b)` — the subtree **overflowed**. It broke into `a` and `b`,
  and handed its middle key `(k, v)` back to the parent to hold between them.

Modelling the promotion as a sum is what keeps it in the types instead of
hiding it in a mutation. An empty position always answers `split(leaf, k, v,
leaf)`, so a new key is always a promotion its parent absorbs:

- a **2-node** parent has room: it absorbs the promotion and becomes a 3-node.
- a **3-node** parent has none — three keys and four children is not a node —
  so it **splits** in turn, sending its own middle key one level further up.
  This can cascade; steps 7 and 13 of the script each split twice in one insert.
- a promotion that reaches the **root** becomes the new root. That is a **root
  grow**, and it is the only way the tree ever gets taller — it gains a level
  everywhere at once, so every leaf stays at the same depth.

## Delete: borrow, merge, and the deficit that climbs

This is the half a 2-3 tree is interesting for, and it runs the same idea
backwards. Removing the only key of a 2-node leaves a node with no key: a
**deficit**, a subtree that came back one level shorter than its siblings. The
parent must repair it before anyone else sees it, and it has exactly two moves
(`tree/delete`):

- **BORROW** — a sibling with two keys can spare one. It sends its nearest key
  up through the parent, and the parent's key comes down into the short child.
  Heights are restored on the spot and nothing climbs. The script borrows from a
  **left** sibling at steps 18 and 20, and from a **right** sibling at step 19.
- **MERGE** — a sibling with only one key cannot spare it. The short child, the
  key between them, and the sibling fuse into a single node. That costs the
  parent a key: fine for a 3-node parent, which simply becomes a 2-node, but a
  **2-node** parent is now itself a node with no key — so the deficit **climbs**
  one level and the parent's parent repairs it in turn.
- a deficit that survives all the way to the **root** makes the root's one
  surviving child the new root. That is a **root shrink** — the only way the
  tree ever gets shorter, and again it loses a level everywhere at once. Step 22
  merges twice, the deficit reaches the root, and the tree drops from height 3
  to height 2.

A key held by an *internal* node is never removed where it sits. It is
overwritten by its in-order successor — the smallest key of the subtree just to
its right — and that successor is then deleted from the bottom, where a key can
actually be taken out. Step 20 deletes key 50, an internal key, this way.

## The audit

After every operation `tree/audit` recomputes four invariants **from the tree
alone**. It reads no flag the insert or delete path set, and takes no shortcut
through the balance it is checking: the depth walk visits every child instead of
following one, and the size walk counts the keys instead of reading the count
the tree stores.

| # | invariant | what breaks it |
| --- | --- | --- |
| a | every leaf is at the same depth | a borrow or merge that rebuilds a node at the wrong height |
| b | keys ascend strictly across an inorder walk | a rotation that moved a key past one it should have stayed behind |
| c | one more child than keys at every node, and a 3-node's two keys ascend | a repair that assembled a node's keys out of order |
| d | the stored size equals the counted size | a merge that dropped a key, or a split that copied one |

**(a) is the one this castle is built around.** A subtly wrong rebalance still
leaves a tree that looks fine — the keys are all there, they are all in order,
every node is well formed — and only the leaf depths give it away. Nothing else
would notice.

Invariant (d) earns its keep from the stored size: the tree *claims* a size, and
the audit counts the keys actually present and compares. Step 14 re-inserts an
existing key (40), which must replace the payload and leave the size alone.

The arity half of (c) cannot fail against the node type as declared — two keys
and three children are a 3-node's *shape*. The audit recomputes it from counts
anyway, so all four are reported the same way; the half that a bad rebalance
really can break is the ascending order of a 3-node's two keys.

## The operation script

`tree/script` holds the script as an ordinary cons list, built in the order it
runs. Fourteen inserts (payloads are bird names), then eight deletes:

```text
insert  50 30 70 20 40 60 80 10 35 45 55 65 25   then 40 again, as "harrier"
delete  10 25 20 45 30 50 33 35
```

The re-insert of 40 replaces a payload without changing the size. The delete of
33 is a key that was never there. Between them the script drives every
structural event the tree has: a promotion out of a leaf, single splits,
cascading splits, three root grows, a borrow from a left sibling, a borrow from
a right sibling, merges, a deficit that climbs two levels, and a root shrink.

## Reading the output

One line per operation:

```text
<step> <op> <key>  h=<height> n=<keys>  <audit>  <outcome and rebalances>
```

```text
 7 ins  80  h=3 n= 7  ok  added split=2 root-grow=1
19 del  30  h=3 n= 8  ok  removed borrow-right=1 merge=1
22 del  35  h=2 n= 6  ok  removed merge=2 root-shrink=1
```

- `h` is the tree's height and `n` the number of keys it holds *after* the
  operation.
- `<audit>` is `ok` when all four invariants hold. Anything else — `BAD-depth`,
  `BAD-order`, `BAD-arity`, `BAD-size` — names the first that did not, and is a
  bug in the rebalancing rather than a fact about the data. It must be `ok` on
  every line.
- The tail says what the operation did (`added`, `replaced`, `removed`,
  `absent`) and then names each rebalance that fired to get there.

After the transcript the program prints the final tree as an indented structure
— **including its leaves**, deliberately: every `leaf` line lands at the same
indent exactly when the tree is balanced, so the headline invariant is there to
see as a shape and not only to read as a word. Then the keys in order, the four
invariants once more, and totals for every split, borrow, merge, root grow and
root shrink the script caused.

## Module layout

| module | what it holds |
| --- | --- |
| `tree/node` | the node sum, the tree handle and its stored size, height, lookup |
| `tree/insert` | the `fitted` / `split` sum and the bottom-up split |
| `tree/delete` | the `held` / `deficit` / `missing` sum, the five repairs, the successor swap |
| `tree/audit` | the four invariants, recomputed from the tree |
| `tree/render` | the indented drawing and the inorder listing |
| `tree/script` | the operation script and the transcript |
| `tree/tally` | the six structural events an operation can cause |
| `tree/text` | column padding, built without any host string inspection |

What this adds to the corpus: the corpus's first self-balancing search tree —
bottom-up splits promoting keys, deletion rebalancing by borrow and merge with
deficits propagating to a shrinking root, and an invariant audit recomputed from
the tree alone after every operation. Where the existing tree-shaped castles
walk or evaluate a structure someone else's code built, this one *maintains* a
structural invariant under both insertion and deletion and then refuses to take
its own word for it: the height-changing paths (a promotion that reaches the
root, a deficit that climbs to it) are exercised in both directions in one run,
and every one of the twenty-two operations is followed by a check that recomputes
leaf depth, key order, node arity and size from the tree itself. It is also the
corpus's first program to model both a rebalance result and a promotion as
explicit sums (`held` / `deficit` / `missing`, `fitted` / `split`) rather than as
mutation, so the structural events a 2-3 tree performs are visible in the types
and countable in the output.
