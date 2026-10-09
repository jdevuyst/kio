#!/bin/sh
set -eu

observed_work=$(mktemp -d "${TMPDIR:?}/kio-observed.XXXXXX")
trap 'rm -rf "$observed_work"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
find -L workdir -path workdir/out -prune -o -type f -name '*.kio' -print |
  while IFS= read -r source; do
    relative=${source#workdir/}
    mkdir -p "$observed_work/$(dirname "$relative")"
    cp "$source" "$observed_work/$relative"
  done
cd "$observed_work"
"$KIO_BIN" build "$KIO_TARGET"
"$KIO_RUNNER" --protocol @PROTOCOL@ "out/$KIO_TARGET" >runtime.stdout

result=$(tail -n 1 runtime.stdout)
case "$result" in
  ''|*[!0-9]*|0[0-9]*) printf 'invalid generated integer observation: %s\n' "$result" >&2; exit 1 ;;
esac
[ "${#result}" -le 7 ] && [ "$result" -le 1000000 ] || exit 1
sed '$d' runtime.stdout
printf '\nequiv kio_gen_runtime_observation { kio_gen_observed(); %s(I32) }\n' "$result" >>prog.kio
if "$KIO_BIN" test >equiv.stdout 2>equiv.stderr; then
  :
else
  status=$?
  cat equiv.stdout equiv.stderr >&2
  exit "$status"
fi
