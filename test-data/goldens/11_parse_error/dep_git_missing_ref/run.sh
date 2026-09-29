#!/bin/sh
# A `source { git ... }` dependency block without a `ref` is malformed:
# a git dependency requires both `git "<url>";` and `ref "<rev>";`. The
# `.dep.kio` parser rejects it at the parse/load tier (exit 11), before
# any clone is attempted — so the case is hermetic (no network).
#
# The deliberately-malformed `lib.dep.kio` is not parseable, so the case
# carries SKIP_KIO_FMT_CHECK (a `kio fmt` round-trip can't canonicalize a
# file that doesn't parse). Its regular module source (`m.kio`) is
# Kio'-shaped, so IS_KIO_PRIME holds; SKIP_KIO_PRIME_RUN opts out of the
# kio-prime run impl (the failure is a parse error reached by `kio
# check`, not a runnable program).
set -u
cd workdir || exit 1
"$KIO_BIN" check
