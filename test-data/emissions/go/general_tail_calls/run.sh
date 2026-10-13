#!/bin/sh
# SUBJECT: Opaque callable values retain captured owners and reusable value/type entries on a bounded Go stack.
# CONTRACT: specs/backends/go.md § Abstract values and typed views
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-tail-calls.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp host/main.go host/go.mod "$scratch/"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
cd "$scratch"
go build -o host-probe.exe .
printf 'default: '
./host-probe.exe
go build -gcflags='all=-N -l' -o host-probe.exe .
printf 'unoptimized: '
./host-probe.exe
