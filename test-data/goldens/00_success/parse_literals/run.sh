#!/bin/sh
# Pins the four literal shapes documented in `specs/language.md`
# (Literals): String (with every JSON-style escape), Bool (.t/.f),
# Integer (signed/unsigned across sizes, with `_` digit separators),
# and Float (with a fractional part, optional exponent, and digit
# separators in mantissa and exponent). Every literal carries an
# explicit `(Type)` annotation naming the role-bearing type
# declared in the package file — Kio' admits no bare-literal form, so
# tier 1 of `specs/language.md` § Literals is the only tier this case
# exercises.
set -u
cd workdir || exit
"$KIO_BIN" check
