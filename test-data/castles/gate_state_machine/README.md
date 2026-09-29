# gate_state_machine

This castle models a deterministic security gate controller. The gate advances
through a fixed twelve-tick incident schedule: wake, valid entry, passage,
obstruction, recovery, repeated invalid badges, lockout, timeout, emergency
release, and reset.

The package reads no stdin. The checked-in fixture is the event schedule
encoded in the Kio source so the run is deterministic across backends. Stdout
contains the compact event transcript, the final state and counters, and one
invariant line showing whether the aggregate counters stayed coherent.

What this adds to the corpus: this is a medium game/state-machine/simulation
castle using a product-shaped state record, several phase and event codes,
host `rec(loop)` ticking, numeric counters, boolean status rendering, and a
testapi-conformed arithmetic-collection host surface.
