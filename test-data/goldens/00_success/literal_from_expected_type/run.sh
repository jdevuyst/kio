#!/bin/sh
# Tier 2 of literal resolution (specs/language.md § Literals): a
# bare literal with no annotation takes its type from the
# expected type the surrounding position supplies. The package
# declares two int-shaped host types, so tier 3 (unique candidate)
# never fires — every bare literal here is resolved purely by the
# position. Covers function-return type and call-argument /
# parameter type. (Kio `let` bindings carry no annotation, so the
# "let annotation" position does not exist in the language.)
set -u
cd workdir || exit
"$KIO_BIN" check
