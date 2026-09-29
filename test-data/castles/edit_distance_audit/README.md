# edit_distance_audit

This castle computes a Levenshtein edit-distance audit between two fixed token
sequences. The tokens are embedded in the Kio source as `I32` codes, so the
program stresses dynamic programming and mutable host arrays without mixing a
text parser into the fixture.

There is no `input.stdin`; the sequence data lives in
`edit_distance_audit/tokens.kio`. Stdout prints the sequence lengths, each
rolling DP row with a checksum, the final matrix checksum, the final distance,
and the classification selected from that distance.

What this adds to the corpus: this is a medium dynamic-programming castle using
the `testapi-array` protocol, polymorphic host arrays, recursive loop lowering,
mutable row state, numeric formatting, string concatenation, and string
equality, built on all five declared backends. It is deliberately no-stdin so
the corpus gets an array-heavy algorithm whose fixture is ordinary source data
rather than a replay transcript.
