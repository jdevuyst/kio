# ledger_rule_audit

`ledger_rule_audit` is a compact stdin replay for a small ledger rule
audit. The program reads command lines from `input.stdin`, applies them to an
in-memory ledger, and prints a transcript plus a final summary.

The fixture is line-oriented and ASCII-only:

- `credit`, `debit`, and `fee` commands consume the following line as a signed
  base-10 amount parsed with `string_to_int`.
- `balance` prints the current balance without changing state.
- `end` stops the replay.
- Unknown commands and invalid amount lines are counted as invalid entries.

The grammar intentionally uses whole-line command tokens and whole-line numeric
amounts. It does not need string slicing, length checks, or byte inspection,
which keeps it within the `testapi-compute` runner surface.

Stdout is a self-contained transcript. It shows every applied ledger rule, each
invalid-input decision, and the final aggregate counts and totals.

## What this adds to the corpus

This castle adds a stdin command-replay utility shape with numeric parsing,
state updates, exact-line command dispatch, and backend runner coverage for
`testapi-compute` across JS, TS, Rust, Go, and Swift. Its main stress is
composed host-boundary use: line input, `String | .` and `I32 | .` matching,
string equality, string concatenation, arithmetic, recursive replay, and
readable output without relying on the runner to echo fixture input.
