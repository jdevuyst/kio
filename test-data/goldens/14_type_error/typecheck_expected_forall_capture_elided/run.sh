#!/bin/sh
# The lambda's inner `A` must not capture the expected function's outer `A`.
# The source parses as Kio', but type-argument elision is a full-Kio rule;
# the explicit-argument sibling isolates the standalone Kio' path.
set -u
cd workdir || exit
"$KIO_BIN" check
