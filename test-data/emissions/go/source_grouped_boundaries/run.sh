#!/bin/sh
# SUBJECT: Go package-qualified roles and source-grouped declaration, callback, and newtype interfaces preserve host effect order.
# CONTRACT: specs/backends/go.md § Package API and § FFI surface
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.go-source-groups.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

mkdir -p "$scratch/driver/artifact" "$scratch/driver/.gocache" "$scratch/driver/.gotmp"
cp out/go/*.go "$scratch/driver/artifact/"
cp "$scratch/host/main.go" "$scratch/driver/main.go"
printf 'module driver\n\ngo 1.26\n' >"$scratch/driver/go.mod"
cd "$scratch/driver"
GOENV=off GOEXPERIMENT='' GOTOOLCHAIN=local GOWORK=off \
  GOPROXY=off GOSUMDB=off GOTELEMETRY=off CGO_ENABLED=0 \
  GOCACHE="$scratch/driver/.gocache" GOTMPDIR="$scratch/driver/.gotmp" \
  TEST_TELEMETRY_DIR="$scratch/driver/.telemetry" go run .
