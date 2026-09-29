# dict_inventory_reconcile

This castle models a warehouse inventory reconciliation pass. The Kio source
contains fixed catalogue rows, a ledger snapshot, adjustment records, and a
physical count snapshot. It builds dictionary values through the `dict` POC
package, looks up expected and observed counts by SKU, applies adjustments, and
prints a compact discrepancy report.

There is no `input.stdin`; all fixture data is in source. `stdout` prints one
line per known catalogue item, one line for unknown observed SKUs, then summary
counts for matching, missing, skewed, and unknown rows.

What this adds to the corpus: a dependency-integrated application shape built
around the dictionary POC, with domain records, fixed business fixtures,
loop-driven list/dictionary walks, labeled report rows, and cross-backend
rendering through the no-stdin `testapi-arith-collection` protocol.
