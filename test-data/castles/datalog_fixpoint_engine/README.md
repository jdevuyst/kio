# datalog_fixpoint_engine

A Datalog evaluator: a deductive database of ground facts, a set of Horn
clauses over them, and a bottom-up fixpoint that derives everything the
clauses entail. The program then answers four queries against the
saturated result.

There is no `input.stdin` — the facts, the rules, and the queries are all
fixtures in `datalog/program`. Everything on stdout is the program's own
output.

## The extensional database (EDB)

The given facts are a six-station one-way transport network:

```text
alder --4--> birch --3--> dover --5--> elm --6--> fen
    \                       ^
     \--7--> cedar --2-----/
```

```prolog
link(alder, birch, 4).      station(alder, north).     hub(alder).
link(alder, cedar, 7).      station(birch, north).     hub(dover).
link(birch, dover, 3).      station(cedar, mid).       depot(cedar).
link(cedar, dover, 2).      station(dover, mid).       depot(fen).
link(dover, elm, 5).        station(elm, south).
link(elm, fen, 6).          station(fen, south).
```

Sixteen facts. `link(from, to, cost)` is a hop, `station(id, zone)` places
a station in a fare zone, and `hub` / `depot` are the unary facts.

Datalog has no arithmetic, so the `4` in `link(alder, birch, 4)` is a
constant *symbol* like `alder` — not a number the engine can add. The host
surface this package declares has no string comparison either, so every
predicate, constant, and variable is an I32 code and the names above exist
only for printing.

## The rules (IDB)

```prolog
reach(X, Y)    :- link(X, Y, C).
reach(X, Z)    :- link(X, Y, C), reach(Y, Z).
oddhop(X, Y)   :- link(X, Y, C).
oddhop(X, Z)   :- link(X, Y, C), evenhop(Y, Z).
evenhop(X, Z)  :- link(X, Y, C), oddhop(Y, Z).
samezone(X, Y) :- station(X, Z), station(Y, Z).
hubserved(Y)   :- hub(X), reach(X, Y).
depotzone(Y)   :- depot(X), samezone(X, Y).
```

- `reach` is the transitive closure of `link`. Its recursive clause joins
  two goals on the shared variable `Y`, and its head variable `Z` is
  contributed *only* by the second goal.
- `oddhop` and `evenhop` are **mutually recursive**: a path of odd hop
  count is one hop plus an even one, and the reverse. Neither can be
  saturated before the other, so the fixpoint has to grow them together.
  On this acyclic network every reachable pair has exactly one hop parity,
  so `oddhop` (9 facts) and `evenhop` (5 facts) partition `reach` (14).
- `samezone` joins two extensional goals on the zone variable `Z`, which
  appears in neither head slot.
- `hubserved` and `depotzone` conclude unary facts whose only variable
  comes from the second goal. `depotzone`'s second goal is itself derived,
  so it cannot fire until `samezone` exists.

## Semi-naive evaluation, in two sentences

A round fires every rule against the database, adds the conclusions that
are not already there, and repeats until a round adds nothing. The
**naive** strategy lets every goal range over the whole database each
round, so it re-derives everything it already knows; the **semi-naive**
strategy keeps the facts that were new last round — the *delta* — and
admits only derivations that consume at least one of them, because
anything derivable without a new fact was already derived in an earlier
round.

Reading a rule body left to right, some goal is the *first* one to take a
new fact: everything before it took an old fact, and everything after it
is free. Enumerating derivations by that first position reaches each of
them exactly once, which is what `datalog/unify`'s `solve_delta` does.

## Reading the round trace

Both strategies print the same three columns:

```text
  round  derivations  new facts
      1           24         24
      2           17         15
      3           10          8
      4            5          2
      5            1          0
```

- **derivations** — complete solutions of a rule body this round, i.e. how
  many candidate head facts the rules produced, before duplicates are
  discarded.
- **new facts** — how many of those conclusions were not already known.

The `new facts` column falling to zero is the termination witness: a round
that adds nothing can never make a later round add something, so the
fixpoint is reached. The `derivations` column shrinking round after round
is what semi-naive evaluation buys — under the naive strategy the same
column *grows* (24, 41, 51, 56, 57) because every round re-derives the
whole database from scratch.

The two runs are then compared directly: 57 derivations against 229, a
saving of 172, and `same fixpoint: true` — both strategies land on the
same 65 facts (16 given, 49 derived). That identity is the correctness
proof for the cheaper one, and every count here is small enough to check
by hand.

## Queries

A query is an atom with variables; its answers are all the substitutions
that match it against the saturated database, reported in derivation order
(the order the facts entered the database):

```prolog
?- reach(alder, Y).      Y = birch | cedar | dover | elm | fen
?- link(X, dover, C).    X = birch, C = 3 | X = cedar, C = 2
?- evenhop(X, fen).      X = dover | alder
?- depotzone(Y).         Y = cedar | dover | elm | fen
```

`alder` is reachable *from* nowhere, so it never appears as a `hubserved`
or `reach` answer target — the deductions are as sharp as the graph.

## Module map

| module | what it holds |
| --- | --- |
| `datalog/list` | the one collection: a generic cons list, with the polymorphic walks every other module reuses |
| `datalog/symbols` | the symbol table — predicate, constant, and variable codes, and the names they print as |
| `datalog/atom` | terms (constant or variable) and atoms (a predicate plus an argument vector) |
| `datalog/subst` | the substitution environment: variable code to constant code |
| `datalog/unify` | one-way matching of a goal against a ground fact, and the two body solvers — naive and semi-naive |
| `datalog/program` | the EDB, the eight Horn clauses, and the queries |
| `datalog/eval` | the round loop: absorb, count, iterate to a fixpoint, twice |
| `datalog/query` | answering a query and choosing which variables an answer reports |
| `datalog/report` | rendering the program back into Datalog notation, and the printed report |

What this adds to the corpus: the corpus's first deductive database —
Horn-clause rules evaluated to a bottom-up fixpoint by semi-naive
iteration, with variable unification, multi-atom joins, transitive
closure, and a naive-vs-semi-naive derivation count that must reach the
same fixpoint. It exercises shapes no other castle does: a solver whose
recursion runs *across* two `rec(loop)` groups (a body walk and a database
scan calling into each other), a `rec(loop)` group of three members with
distinct signatures, polymorphic higher-order list walks (`concat_map`,
`fold`, `any`, `all`) driving the join, `pub literal` symbol tables, and
the same list `newtype` instantiated at six different element types.
