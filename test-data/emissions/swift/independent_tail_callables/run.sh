#!/bin/sh
# SUBJECT: Independent Swift modules share typed callback identity through opaque storage and native bridges.
# CONTRACT: specs/backends/swift.md § Callback transport and native bridges.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.swift-independent-tail.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R host "$scratch/host"
(
  cd "$scratch/workdir"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
  cd peer
  "$KIO_BIN" build "$KIO_TARGET"
)
first="$scratch/workdir/out/swift"
second="$scratch/workdir/peer/out/swift"
native="$scratch/native"
mkdir -p "$native/.module-cache"
swiftc -Onone -module-name KioRuntime \
  -module-cache-path "$native/.module-cache" \
  -emit-module -emit-module-path "$native/KioRuntime.swiftmodule" \
  -emit-library -static -o "$native/libKioRuntime.a" \
  "$first/KioRuntime/KioRuntime.swift"
swiftc -Onone -module-name CallbackA -I "$native" \
  -module-cache-path "$native/.module-cache" \
  -emit-module -emit-module-path "$native/CallbackA.swiftmodule" \
  -emit-library -static -o "$native/libCallbackA.a" "$first"/*.swift
swiftc -Onone -module-name CallbackB -I "$native" \
  -module-cache-path "$native/.module-cache" \
  -emit-module -emit-module-path "$native/CallbackB.swiftmodule" \
  -emit-library -static -o "$native/libCallbackB.a" "$second"/*.swift
swiftc -Onone -I "$native" -L "$native" \
  -module-cache-path "$native/.module-cache" \
  -lCallbackA -lCallbackB -lKioRuntime \
  "$scratch/host/driver.swift" -o "$native/driver"
"$native/driver"

for control in wrong_callable wrong_nominal private_payload private_constructor; do
  if swiftc -typecheck -I "$native" \
    -module-cache-path "$native/.module-cache" \
    "$scratch/host/$control.swift" >"$scratch/$control.log" 2>&1; then
    echo "invalid Swift host unexpectedly compiled: $control" >&2
    exit 1
  fi
done
