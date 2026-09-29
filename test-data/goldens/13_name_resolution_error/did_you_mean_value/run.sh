#!/bin/sh
# A misspelled value reference (`greetin` for the in-scope `greeting`)
# is the standard unbound-name error (exit 13), enriched with a
# `did you mean …?` near-miss suggestion. The stderr-grep checks the
# unbound-name message and the machine-applicable replacement suggestion.
set -u
cd workdir || exit
"$KIO_BIN" check
