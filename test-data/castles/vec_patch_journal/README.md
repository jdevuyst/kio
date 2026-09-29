# vec_patch_journal

`vec_patch_journal` replays a fixed patch journal over a persistent vector from
the `vec` POC package. The program starts with a small vector, applies a mix of
in-bounds and out-of-bounds set/read operations, records checkpoints into a
second vector, and prints the final counts and checksums.

This castle does not read stdin; the patch sequence is part of the Kio source.
`run.args` selects the `testapi-arith-collection` runner protocol. The stdout
report names the final vector length, successful reads, misses, vector
checksum, checkpoint checksum, combined report value, and whether the replay
produced the expected read/miss counts.

What this adds to the corpus: a dependency-integrated data-structure workflow
that exercises the `vec` POC through path dependencies, persistent updates,
optional reads, vector folding, record labels, and a loop-driven replay state
machine with no stdin fixture.
