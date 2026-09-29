# Replica vector-clock lab

Three replicas maintain a persistent dictionary from integer keys to lists of
causally maximal versions. Each version carries a three-component vector
clock. Local writes advance the author's component, message delivery merges
observed clocks, and concurrent values remain available for explicit
resolution. Reads return typed missing-key or conflict failures; resolution
also rejects an absent choice and a key with no conflict.

The fixture is in `workdir/lab/scenario.kio` and reads no stdin. Replica A
writes values 10 then 11 at key 10; B independently writes 20 at key 10 and
200 at key 20; C independently writes 30 at key 10. Forward, reversed, and
shuffled message lists include duplicate deliveries. A then selects B's value
20 to resolve key 10, and B subsequently updates key 20 to 201.

Stdout prints clock relations, the retained versions in lexicographic clock
order, typed read and resolution outcomes, and convergence checks. Before
resolution, key 10 retains clocks `[0,0,1]`, `[0,1,0]`, and `[2,0,0]`;
`[1,0,0]` is superseded. All replicas have observed `[2,2,1]`. Resolution
advances A to `[3,2,1]`, so delayed originals cannot restore old values. The
final write advances B to `[3,3,1]`. A saved pre-resolution snapshot still
contains its three conflicting versions.

The `dict`, `list`, `result`, and `elab` packages supply keyed storage,
version sets and message folds, recoverable outcomes, and surface control
forms. Dependencies are materialized with `kio dep fetch`; collection host
requirements are forwarded through local adapters.

What this adds to the corpus: interacting replica state, partial causal
ordering, idempotent delivery, explicit conflict retention and resolution,
cross-key clock propagation, and persistent snapshots composed from reusable
collections and typed failure handling.
