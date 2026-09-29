#!/bin/sh
# SUBJECT: Go's public facade keeps both callable layers of a nested curried callback across host and export boundaries.
# CONTRACT: specs/backends/go.md § FFI surface
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-nested-curried.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

out="$scratch/workdir/out/go"
stage="$scratch/driver"
mkdir -p \
  "$stage/artifact" \
  "$stage/.gocache" \
  "$stage/.gomodcache" \
  "$stage/.gotmp" \
  "$stage/.telemetry"
cp "$out"/*.go "$stage/artifact/"
cp "$scratch/host/main.go" "$stage/main.go"

{
  printf 'module driver\n\n'
  printf 'go 1.26\n'
} >"$stage/go.mod"

cd "$stage"
GOENV=off GOEXPERIMENT='' GOTOOLCHAIN=local GOWORK=off \
  GOPROXY=off GOSUMDB=off GOTELEMETRY=off CGO_ENABLED=0 \
  GOCACHE="$stage/.gocache" GOMODCACHE="$stage/.gomodcache" \
  GOTMPDIR="$stage/.gotmp" TEST_TELEMETRY_DIR="$stage/.telemetry" \
  TMPDIR="$stage/.gotmp" go run .
