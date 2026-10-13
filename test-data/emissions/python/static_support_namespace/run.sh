#!/bin/sh
# SUBJECT: The fixed Python static support namespace remains separate from a package with the same source name.
# CONTRACT: specs/backends/python.md § Output layout
set -eu
scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-static-support.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
test -f out/python/__kio_pkg_kio_ustatic.py
test -f out/python/__kio_pkg_kio_ustatic/__init__.pyi
test ! -f out/python/kio_static/__init__.pyi
set -- out/python/kio_static/*.pyi
test -f "$1"
printf 'static support namespace\n'
