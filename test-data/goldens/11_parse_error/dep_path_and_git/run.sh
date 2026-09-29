#!/bin/sh
# A Git source may include a manifest path, but a rooted selector is malformed
# and fails at parse time (11), before resolution or any network operation.
# Both required Git fields are present, so the selector is the only defect.
#
# The deliberately-malformed `lib.dep.kio` does not parse, so the case
# carries SKIP_KIO_FMT_CHECK (a `kio fmt` round-trip can't canonicalize a
# file that doesn't parse) and SKIP_DEP_MATERIALIZED (the dependency is
# non-materializable). Its regular module source (`m.kio`) is Kio'-shaped,
# so IS_KIO_PRIME holds and both full and reduced compiler commands must
# reject the malformed package input.
set -u
cd workdir || exit 1
"$KIO_BIN" check
