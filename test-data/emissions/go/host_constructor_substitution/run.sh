#!/bin/sh
# SUBJECT: Declaration-owned Go host carriers preserve concrete and generic compound payloads through Kio calls.
# CONTRACT: specs/backends/go.md, Host record contract and Structural and nominal types.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-substitution.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
mkdir -p "$scratch/host/artifact" "$scratch/host/.gocache" "$scratch/host/.gotmp"
cp "$scratch/workdir/out/go/"*.go "$scratch/host/artifact/"
(
  cd "$scratch/host"
  GOENV=off GOEXPERIMENT='' GOTOOLCHAIN=local GOWORK=off GOPROXY=off GOSUMDB=off \
    GOCACHE="$scratch/host/.gocache" GOTMPDIR="$scratch/host/.gotmp" go run .
)
