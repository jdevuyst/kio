# csv_reconcile

`csv_reconcile` is a compact stdin-driven ledger reconciliation program. It
replays an ASCII fixture one line at a time, keeps a running balance, records
checkpoint counts, rejects overdraft debits, and reports the final accepted
totals.

The fixture is one command per line. `BEGIN`, `CHECK`, and `END` are complete
commands. `CREDIT` and `DEBIT` consume the next line as a signed integer amount;
amount lines are parsed with the runner's `string_to_int` helper. The checked-in
fixture opens a ledger, accepts a credit and debit, records a clean check,
accepts another credit, rejects an overdraft debit, records a review check, and
ends the stream.

Stdout is the program's own transcript. It begins with a short title, prints
each accepted or rejected ledger action with the resulting balance, prints each
check with the current review status, and ends with the final balance, accepted
credit/debit totals, check count, and error count.

What this adds to the corpus: this castle exercises a stdin command replay with
amount-bearing commands split across lines, explicit fallible integer parsing,
`rec(loop)` control flow, repeated `String | .` / `I32 | .` sum dispatch, and
the `testapi-compute` host surface across the JS, TS, Rust, and Go backends.
