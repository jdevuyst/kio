#!/bin/sh
# SUBJECT: Swift retains a removed host method and its inference-only associated type as deprecated optional history without live dispatch.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.swift-deprecated-history.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
rm -rf "$scratch/workdir/out"

(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

out="$scratch/workdir/out/swift"
host_build="$scratch/host-build"

mkdir -p "$host_build/.module-cache"
swiftc \
  -Onone \
  -module-name App \
  -module-cache-path "$host_build/.module-cache" \
  -emit-module \
  -emit-module-path "$host_build/App.swiftmodule" \
  -emit-library \
  -static \
  -o "$host_build/libApp.a" \
  "$out"/*.swift
swiftc \
  -Onone \
  -module-cache-path "$host_build/.module-cache" \
  -I "$host_build" \
  -L "$host_build" \
  -lApp \
  "$scratch/host/driver.swift" \
  -o "$host_build/driver"
"$host_build/driver"
