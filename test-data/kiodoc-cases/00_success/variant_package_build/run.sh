#!/bin/sh
# variant=package routes the snippet body through the package-file
# parser. A package file with a build block validates as
# success.
set -u
"$KIO_BIN" doc check
