#!/bin/sh
# A snippet declaring `check_exit_code=14` whose body typechecks
# cleanly: `kio check` exits 0, contradicting the declaration, so the
# run fails.
#
# The snippet was accepted, so there is no `kio check` diagnostic to
# report and the validator must not emit an empty
# `--- kio check diagnostic ---` section. Asserting that absence needs
# the rare byte-identical `expected.stderr` policy — a `.grep` pins what
# appears, not what must not. Nothing here cites a scratch path, so the
# whole stream is deterministic.
set -u
"$KIO_BIN" doc check
