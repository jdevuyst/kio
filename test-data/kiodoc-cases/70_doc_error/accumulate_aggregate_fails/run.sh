#!/bin/sh
# An accumulating harness whose aggregate program fails to
# typecheck surfaces as one DocError under the harness span. The
# per-member bodies are individually well-formed; only their
# combination triggers the failure (declaring the same function
# twice across two members).
set -u
"$KIO_BIN" doc check
