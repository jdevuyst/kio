# river_bytecode_vm

`river_bytecode_vm` interprets a fixed bytecode script for the classic
wolf/goat/cabbage river crossing. The bytecode has opcodes for waiting,
moving one cargo item with the farmer, ferrying the farmer alone, checking
the safety invariant, branching when the crossing is complete, and halting.

There is no `input.stdin`. The checked-in fixture is the bytecode table
compiled into the Kio modules, and the runner protocol is
`testapi-arith-collection`. The full host environment for that protocol is
declared under the `testapi` module tree.

Stdout prints a compact execution summary: the script name, halt status,
final bank of each actor, counters for moves/waits/alerts/ticks, a checksum
of the VM trace, and final safety/done booleans. The result is meant to be
readable without inspecting the bytecode source.

What this adds to the corpus: this is a medium bytecode-VM/interpreter castle
over a small river-crossing control-flow script. It stresses a multi-module
fixed instruction table, label-heavy VM state, `rec(loop)` interpretation,
conditional dispatch, invariant checks, and compact report rendering under
the `testapi-arith-collection` protocol without stdin, arrays, raw
intrinsics, or POC dependencies.
