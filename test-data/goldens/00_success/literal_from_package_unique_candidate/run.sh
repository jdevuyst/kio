#!/bin/sh
# Tier 3 of literal resolution (specs/language.md § Literals): a
# bare literal in a synthesis position — a `let` right-hand side,
# which carries no expected type — resolves to the package's unique
# role-bearing type whose shape it admits. The package exports
# exactly one int-shaped and one str-shaped type, so each bare
# literal has a single tier-3 candidate.
set -u
cd workdir || exit
"$KIO_BIN" check
