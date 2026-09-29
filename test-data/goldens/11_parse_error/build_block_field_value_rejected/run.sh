#!/bin/sh
# A build-block target entry writes its field as `out = "out/js/";`. The
# `=` lands where `parse_block_field_value` expects a field value (the
# unit literal `()`, a string literal, a number, a boolean, or a bare
# word), so the parser rejects it with exit 11. Exercises that
# function's catch-all arm — the build-block analogue of the deleted
# `source`-block coverage.
set -u
cd workdir || exit
"$KIO_BIN" check
