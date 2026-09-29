#!/bin/sh
# Passing a bare function *value* (`ident : . -> .`) where `. -> W` is
# expected. With no lambda literal at the call site there is no body to
# flow an expected type into, so the typer compares the whole function
# types and rejects on the differing return type (exit 14).
set -u
cd workdir || exit
"$KIO_BIN" check
