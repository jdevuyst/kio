#!/bin/sh
# Two numeric literals at the same role-bearing type `I32`
# but with different digit strings (`42` vs `7`). They residualize
# to distinct literal-atom NFs that disagree on digits, per
# specs/formal/equiv.md § 4.5 "42^i32 ≄ 7^i32". The runner keeps
# literals as `(digits, role)` atoms and never evaluates them
# arithmetically (the numeric type is opaque), so the
# distinction is purely on the digit string here. Exits 50 from
# the `5x` test-error tier.
set -u
cd workdir || exit
"$KIO_BIN" test
