# inventory_cycle_counter

This castle audits a fixed inventory cycle count across four bins. Each bin has
an expected and observed count embedded in Kio source; there is no stdin fixture
and no dependency.

Stdout reports the per-bin status, the absolute discrepancy total, and the
number of ok/missing/overage bins. The implementation uses labeled records for
bin facts and integer comparisons to classify the count drift.

What this adds to the corpus: a state-audit workflow over record-shaped domain
data on the `testapi-arith-collection` protocol. It exercises negative integer
deltas, absolute-value branching, formatted reporting, and multiple composed
domain modules.
