# Hierarchical test report

Consumes a test executor's event stream, assembles nested suite trees, and
renders suite totals and fully qualified failure paths. Each open suite keeps
its own direct failures and completed children. Closing a child propagates its
pass, fail, and skip counts into its parent; completed roots form the report.

`input.stdin` consists of four whole ASCII lines per event: tag, name, message,
and decimal count. Tags are `suite`, `end`, `pass`, `fail`, and `skip`. A suite
opens with count zero. Test counts are positive multiplicities; an `end` count
asserts the suite's total, including descendants. Messages are displayed for
failures. The fixture uses `_` for messages that have no report content.

A mismatched close leaves the current suite open so a later correct close can
recover. A total mismatch reports a diagnostic and closes the suite using its
computed counts. Invalid records and tests outside a suite do not change the
tree. EOF reports unfinished suites, which do not contribute to the completed
roots' report. Domain diagnostics are ordinary output; the process exits zero.

The fixture includes a three-level compiler suite with passing, failing, and
skipped tests, then a recoverable malformed stream and a truncated event.
The report shows suites in opening order, direct failure paths below their
suite, aggregate totals for completed roots, and diagnostics in encounter order.

The modules separate event decoding, tree data, stack aggregation, and report
rendering. The reusable List library supplies stack cells, immutable child
lists, folds, and the rendering frontier; its host functions are rehosted onto
the package's existing capabilities. The elaborator library provides ordinary
conditionals, sum construction, and exhaustive matching.

What this adds to the corpus: hierarchical event-to-document assembly with
deferred parent aggregation, recursive suite values inside public lists,
recovery after structural errors, and an iterative tree renderer over stdin
data. The output depends on suite nesting and accumulated child results as
well as the individual event records.
