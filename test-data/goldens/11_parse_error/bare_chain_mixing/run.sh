#!/bin/sh
# Bare same-operator chains are accepted in surface Kio
# (`A & B & C`, `A | B | C`), but mixing `&` and `|` in one
# bare chain still requires explicit parentheses — exit 11
# (parse error). Mirrors the inside-paren mixing rule.
set -u
cd workdir || exit
"$KIO_BIN" check
