#!/bin/sh
# A private elaborator declared earlier in a module is in that module's
# lexical bang-call scope. `pub` controls export; it is not required for a
# declaration to be used by a later item in its own module.
set -u
cd workdir || exit
"$KIO_BIN" check
