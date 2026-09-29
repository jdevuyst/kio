#!/bin/sh
# An unknown directive keyword in a /// doc-comment is a runner error
# (exit 70). The keywords `@eval` and `@signaure` (typo) are not in the
# recognized vocabulary and must produce diagnostics.
set -u
"$KIO_BIN" doc check
