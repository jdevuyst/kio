#!/bin/sh
# SUBJECT: Go compiles unchanged and live-only hosts while preserving sealed removed signature aliases.
set -eu

case_dir=$(pwd)
scratch=$(mktemp -d "${TMPDIR:?}/emissions.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
cp -R workdir/. "$scratch/"
cp -R "$case_dir/host" "$scratch/host"
rm -rf "$scratch/out"

cd "$scratch"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

stage="$scratch/retained-host"
mkdir -p \
  "$stage/app" \
  "$stage/.gocache" \
  "$stage/.gomodcache" \
  "$stage/.gotmp" \
  "$stage/.telemetry" \
  "$stage/.tmp"
cp out/go/*.go "$stage/app/"
cp "$scratch/host/go.mod" "$stage/go.mod"
cp "$scratch/host/main.go" "$stage/main.go"

(
  cd "$stage"
  GOENV=off \
    GOEXPERIMENT='' \
    GOTOOLCHAIN=local \
    GOWORK=off \
    GOPROXY=off \
    GOSUMDB=off \
    CGO_ENABLED=0 \
    GOCACHE="$stage/.gocache" \
    GOMODCACHE="$stage/.gomodcache" \
    GOTMPDIR="$stage/.gotmp" \
    TEST_TELEMETRY_DIR="$stage/.telemetry" \
    TMPDIR="$stage/.tmp" \
    go run .
)
