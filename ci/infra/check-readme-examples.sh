#!/bin/sh
# Root README Kio examples are complete modules, checked independently.
set -eu

if [ "$#" -ne 2 ]; then
  printf 'Usage: check-readme-examples.sh <README.md> <kio-binary>\n' >&2
  exit 2
fi
readme=$1
kio=$2
scratch=$(mktemp -d)
cleanup_readme_examples() {
  cleanup_status=$?
  trap '' HUP INT TERM
  trap - EXIT
  rm -rf "$scratch"
  exit "$cleanup_status"
}
trap cleanup_readme_examples EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# Track other-language fences too, so their literal text never becomes an example.
awk -v scratch="$scratch" '
  function fail(message) {
    print FILENAME ":" NR ": " message > (scratch "/error")
    failed = 1
    exit 1
  }
  {
    fence = $0
    sub(/^ ? ? ?/, "", fence)
    if (marker != "") {
      if (substr(fence, 1, 1) == marker && fence ~ /^(```+|~~~+)[ \t]*$/) {
        closing = fence
        sub(/[ \t]+$/, "", closing)
        if (length(closing) >= width) {
          if (example) {
            close(body)
            if (module == "") fail("Kio example needs a complete module declaration")
            print count, module, opening > (scratch "/inventory")
          }
          marker = ""
          next
        }
      }
      if (example) {
        print $0 > body
        if ($0 ~ /^[ \t]*module[ \t]+[[:alnum:]_]+(\/[[:alnum:]_]+)*[ \t]*;/) {
          module = $0
          sub(/^[ \t]*module[ \t]+/, "", module)
          sub(/[ \t]*;.*/, "", module)
        }
      }
      next
    }
    if (fence ~ /^(```+|~~~+)/) {
      marker = substr(fence, 1, 1)
      run = fence
      sub(/[^`~].*$/, "", run)
      width = length(run)
      info = substr(fence, width + 1)
      sub(/^[ \t]+/, "", info)
      example = info ~ /^kio([ \t]|$)/
      if (example) {
        count++
        opening = NR
        module = ""
        body = scratch "/source-" count
      }
    }
  }
  END {
    if (failed) exit 1
    if (marker != "" && example) fail("unclosed Kio example")
    if (!count) fail("no Kio examples found")
  }
' "$readme" || {
  if [ -f "$scratch/error" ]; then
    cat "$scratch/error" >&2
  fi
  exit 1
}

while read -r index module line; do
  work=$scratch/example-$index
  source=$module.kio
  mkdir -p "$work/$(dirname -- "$source")"
  mv "$scratch/source-$index" "$work/$source"
  printf 'README example: %s:%s\n' "$readme" "$line"
  (
    cd "$work"
    "$kio" check
    "$kio" fmt --check "$source"
  )
done <"$scratch/inventory"
