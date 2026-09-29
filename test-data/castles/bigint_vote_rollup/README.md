# bigint_vote_rollup

This castle models a fixed vote-and-ledger rollup for three districts. It keeps
vote margins in signed `I64`, sealed ballot batches in unsigned `U64`, and
ledger balances in signed `I128`, then prints a compact report.

The case reads no stdin. `run.args` selects the `testapi-bigint` runner
protocol, whose host supplies the wide integer roles, arithmetic helpers,
formatters, and `print`.

Stdout is the report transcript: one row per district followed by a final
rollup row. Each row prints the signed margin, unsigned ballot count, and
signed ledger balance as decimal text.

What this adds to the corpus: a compact numeric backend-breadth castle focused
on wide integer role values across every shipping backend. The program combines
multi-module imports, product-shaped ledger rows, signed and unsigned integer
roles, and the `testapi-bigint` host protocol instead of a one-line arithmetic
smoke test.
