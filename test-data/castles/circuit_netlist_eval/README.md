# circuit_netlist_eval

This castle models a fixed combinational circuit as a small netlist with four
input pins, two gate layers, one fanout branch, final probes, and consistency
diagnostics. The program evaluates the network through a labeled state record
and a recursive topological pass driven by the runner's `loop` host function.

There is no `input.stdin`: the fixture is the checked-in network state built by
the package itself. Stdout is a compact circuit report. It prints the input
pins, each gate's truth-table result, the fanout branch, final probes, and the
diagnostic booleans plus warning count.

What this adds to the corpus: this is a medium, fixed-fixture symbolic evaluator
for signal propagation rather than route search, bytecode execution, or text
replay. It stresses labeled product state, recursive `rec(loop)` evaluation,
Boolean truth-table helpers, backend emission for JS/TS/Rust/Go, and the
`testapi-arith-collection` runner surface without stdin.
