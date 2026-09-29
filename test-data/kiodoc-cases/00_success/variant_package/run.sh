#!/bin/sh
# variant=package routes the snippet body through the
# package-file parser. A well-formed package-file snippet
# validates as success.
set -u
"$KIO_BIN" doc check
