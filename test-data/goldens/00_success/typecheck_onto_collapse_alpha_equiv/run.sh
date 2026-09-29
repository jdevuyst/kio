#!/bin/sh
# `onto!`'s sum-collapse rule (`A | A → A`) fires when the two arms
# are `equiv`-equal. Pin that `equiv` recognizes alpha-equivalence
# under quantifiers — `[A] A -> A` and `[B] B -> B` are the
# same type via fresh-variable substitution, so collapse succeeds.
#
# `kio test` runs as a script-owned discharge pass: a run.sh case owns
# its own `kio test` (the standard run.args path's auto discharge does
# not apply here). The consumer declares no `equiv` of its own — every
# `equiv` block lives in the imported `elab` dependency, which a
# default `kio test` skips — so the pass asserts a clean load with no
# consumer equiv to discharge (its stdout `no equiv blocks found` is
# discarded so the empty-stdout snapshot still holds).
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check || exit
"$KIO_BIN" test >/dev/null
