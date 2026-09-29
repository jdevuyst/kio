#!/bin/sh
# `kio test` on a package that compiles cleanly but declares zero
# `equiv` blocks. The runner reports "no equiv blocks found in this
# package" and exits 0 — the absence of tests is not itself a test
# failure. SKIP_KIO_PRIME_RUN opts the case out of the kio-prime
# impl, where `kio test` is rejected outright (`equiv` isn't part of
# the Kio' grammar so kio-prime has no test command).
set -u
cd workdir || exit
"$KIO_BIN" test
