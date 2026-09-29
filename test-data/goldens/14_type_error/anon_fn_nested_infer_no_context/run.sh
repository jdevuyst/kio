#!/bin/sh
# The surface form is syntactically admissible — every nested
# `_` parses cleanly in any type-annotation slot. The rejection
# here is purely semantic: a nested `_` placeholder in a value-
# parameter annotation requires a surrounding expected type to
# resolve against. An anonymous lambda literal bound via `let` (or
# used as a call's callee) sits in synthesis-only position — there
# is no expected type pushing in, so the placeholder has nothing
# to resolve. The typer rejects with exit code 14 and a diagnostic
# pointing the user at the call-site / annotated-return paths that
# supply context.
set -u
cd workdir || exit
"$KIO_BIN" dep fetch >/dev/null || exit
"$KIO_BIN" check
