#!/bin/sh
# SUBJECT: Independent Go artifacts exchange typed cooperative callbacks, including Unit, product domains and nested callable results.
# CONTRACT: specs/backends/go.md § Callable transport and native invocation
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-independent-tail.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp host/main.go host/go.mod "$scratch/"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
  cd peer
  "$KIO_BIN" build "$KIO_TARGET"
)
cd "$scratch"
go build -o host-probe.exe .
printf 'default: '
./host-probe.exe
go build -gcflags='all=-N -l' -o host-probe.exe .
printf 'unoptimized: '
./host-probe.exe
