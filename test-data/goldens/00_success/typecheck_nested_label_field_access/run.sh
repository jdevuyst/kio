#!/bin/sh
# A nominal constructor can be used directly as its projector argument;
# its String payload is inferred without an extra annotation.
set -u
cd workdir || exit
"$KIO_BIN" check
