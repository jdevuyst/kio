# route_ticket_pricer

`route_ticket_pricer` is a no-stdin transit and logistics ticket audit. The
fixture is fixed in the source: four named routes, three bulk ticket pools, and
pre-set tax and checksum ledger entries.

The program computes distance fare totals with `I64`, bulk ticket counts with
`U64`, and large audit totals with `I128`. It prints a compact report showing
the route fare cents, bulk counts, gross/tax/checksum audit totals, and the
combined audit total.

There is no `input.stdin`; all data is in the Kio modules. `run.args` selects
the `testapi-bigint` protocol.

What this adds to the corpus: a no-stdin wide-integer reporting program that
uses the `testapi-bigint` runner surface across JS, TS, Rust, Go, Swift, and
Haskell. The composition stresses namespaced host modules, role-specific wide
integer formatting, and backend emission for `i64`, `u64`, and `i128` values in
one coherent audit workflow.
