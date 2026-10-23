#!/bin/sh
# A user elaborator observes the canonical slash-qualified identity rendered by
# __type_display__, even when its caller spells that identity through an alias.
set -u
cd workdir || exit
"$KIO_BIN" check
