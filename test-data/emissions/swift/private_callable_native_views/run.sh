#!/bin/sh
# SUBJECT: Native callbacks and generic nominal projections retain the typed Swift host API.
# CONTRACT: specs/backends/swift.md, Package API and FFI surface.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.swift-native-views.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)

out="$scratch/workdir/out/swift"
host_build="$scratch/host-build"
mkdir -p "$host_build/.module-cache"
swiftc -Onone -module-name NativeViews \
  -module-cache-path "$host_build/.module-cache" \
  -emit-module -emit-module-path "$host_build/NativeViews.swiftmodule" \
  -emit-library -static -o "$host_build/libNativeViews.a" "$out"/*.swift
swiftc -Onone -module-cache-path "$host_build/.module-cache" \
  -I "$host_build" -L "$host_build" -lNativeViews \
  "$scratch/host/driver.swift" -o "$host_build/driver"
"$host_build/driver"
