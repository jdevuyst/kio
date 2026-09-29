#!/bin/sh
# A java `namespace` whose final segment brands to an emitted support
# class (`Shapes`, `KioRuntime`) is a build error (exit 40) naming the
# collision.
set -u
cd workdir || exit
"$KIO_BIN" build
