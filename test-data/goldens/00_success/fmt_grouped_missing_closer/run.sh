#!/bin/sh
set -eu

# A recovered tree-skeleton closer is useful for editor completion, but never
# authorizes malformed ordinary source. Each opener-owning field is exercised.
"$KIO_BIN" test >/dev/null
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP
for fixture in *.input; do
  case "$fixture" in
    *.pkg.input) target="$work/pkg.pkg.kio" ;;
    *) target="$work/app.kio" ;;
  esac
  cp "$fixture" "$target"
  status=0
  "$KIO_BIN" fmt --check "$target" >/dev/null 2>&1 || status=$?
  if [ "$status" -ne 11 ]; then
    printf '%s: expected parse exit 11, got %s\n' "$fixture" "$status" >&2
    exit 1
  fi
done
printf '%s\n' 'missing grouped closers rejected'
