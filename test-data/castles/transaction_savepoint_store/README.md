# Transaction savepoint store

A persistent key/value store processes transaction records from standard input.
Each write prepends an immutable revision to a journal; a deletion is a
tombstone. Reads select the newest revision for a key. Savepoints retain journal
roots, so rollback restores earlier visibility without mutating retained
snapshots. Revision allocation remains monotone across rollbacks and aborts.

Each input record has exactly three ASCII lines: an uppercase opcode, a key or
savepoint name, and a value. Unused fields contain `-`. `PUT` parses the third
line as an integer. The operations are `BEGIN`, `PUT`, `DELETE`, `SAVE`,
`ROLLBACK`, `RELEASE`, `COMMIT`, `ABORT`, `GET`, and `SNAPSHOT`.
Names and keys occupy whole lines; no token or character parser is involved.

A transaction starts from the committed root. Savepoint names must be unique
among active savepoints. Rolling back to a name restores its root, drops younger
savepoints, and keeps the named savepoint. Releasing a name drops it and every
younger savepoint while retaining the current writes. Commit publishes the
working root and clears savepoints; abort restores the committed root.
Missing names, missing deletion keys, malformed integer values, operations
outside a transaction, and unknown opcodes produce readable errors without
changing state.

The fixture releases an inner savepoint before rolling back the outer one,
commits two transactions, rolls back across a deletion and an insertion, and
abandons a third transaction. Queries expose restored and absent keys.
Every transcript line includes the current revision, visible values in newest
write order, active savepoints, and an audit result.

The audit independently replays each immutable journal from oldest to newest
into a flat association list and checks first-hit lookup against that map.
It checks revision ordering, restoration against retained roots, and the full
state signature on rejected operations. Named snapshots retain both their
journal and independently materialized expected values; all are checked after
every command and printed again at the end, including snapshots of abandoned
writes. The final summary reports operations, rejected commands, commits,
allocated revisions, and audit failures.

## What this adds to the corpus

This is a persistent transactional data structure with nested rollback scopes,
tombstone visibility, monotone revision allocation, retained historical roots,
and an independently represented snapshot audit. It composes the reusable list
library with loop-driven journal traversal, whole-line input decoding, and
ordinary sum-based domain results.
