#!/bin/sh
# Inspector selectors resolve local aliases to their provider declarations.
set -eu
cd workdir || exit
printf '%s\n' \
    ':sig first' \
    ':src second' \
    ':refs first' \
    ':sig op _ ++ _' \
    ':refs op _ ++ _' \
    ':sig op_var [% %]' \
    ':refs op_var [% %]' \
    ':quit' | "$KIO_BIN" repl demo/main
