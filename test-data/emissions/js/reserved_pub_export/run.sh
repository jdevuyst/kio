#!/bin/sh
# SUBJECT: JavaScript exposes a Kio public item whose name is a reserved word under its verbatim property key.
set -eu

case_dir=$(pwd)
scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir/. "$scratch/"
rm -rf "$scratch/out"
cp -R "$case_dir/host" "$scratch/host"

cd "$scratch"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
node "$scratch/host/main.mjs"
