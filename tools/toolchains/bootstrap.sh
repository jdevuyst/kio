#!/bin/sh
# Verified mise bootstrap; no shell-profile or account-default changes.
set -eu
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=${1:?installation root required}
platform=${2:?platform required}
version=$(tr -d '\r\n' <"$script_dir/mise.version")
expected=$(awk -v p="$platform" '$1 == p {print $2}' "$script_dir/mise.sha256")
[ -n "$expected" ] || { printf 'error: no verified mise bootstrap for %s\n' "$platform" >&2; exit 2; }
hash() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}';
  else shasum -a 256 "$1" | awk '{print $1}'; fi
}
case "$platform" in windows-*) suffix=.exe ;; *) suffix= ;; esac
binary="$root/bootstrap/$version/mise$suffix"
if [ ! -f "$binary" ] || [ "$(hash "$binary")" != "$expected" ]; then
  mkdir -p "$root/bootstrap/$version"
  download=$(mktemp "$root/bootstrap/download.XXXXXX")
  trap 'rm -f "$download"' EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  curl --fail --location --silent --show-error --retry 2 \
    "https://github.com/jdx/mise/releases/download/v$version/mise-v$version-$platform$suffix" >"$download"
  [ "$(hash "$download")" = "$expected" ] || { printf 'error: mise bootstrap checksum mismatch\n' >&2; exit 1; }
  chmod 755 "$download"
  mv "$download" "$binary"
fi
printf '%s\n' "$binary"
