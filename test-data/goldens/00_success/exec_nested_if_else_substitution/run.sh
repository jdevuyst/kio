#!/bin/sh
# Regression: a nested surface `if`/`else` is well-typed per
# `specs/language.md` § Conditionals. Inner arms produce `(. |
# ())` and `()`; the outer arm picks one of those, so the whole
# expression has type `((. | .) | .)`. The case asserts every
# implementation accepts it without bailing partway through the
# elaboration of the nested form.
set -u
cd workdir || exit
"$KIO_BIN" check
