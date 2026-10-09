#!/bin/sh
# JS only routes this shared build-preflight diagnostic; no emitted host file
# is consumed. The subject is preservation of the compiler's own source input.
set -eu
output_guard_scratch=$(mktemp -d)
trap 'rm -rf "$output_guard_scratch"' 0
trap 'exit 1' 1 2 3 15
cp -R workdir "$output_guard_scratch/workdir"
if [ -f IS_KIO_PRIME ]; then
    cp IS_KIO_PRIME "$output_guard_scratch/IS_KIO_PRIME"
fi
cd "$output_guard_scratch/workdir"
if [ ! -f ../IS_KIO_PRIME ]; then
    "$KIO_BIN" test >/dev/null
fi
"$KIO_BIN" check >/dev/null
cp src/main.kio "$output_guard_scratch/source.expected"
status=0
"$KIO_BIN" build js >"$output_guard_scratch/build.stdout" 2>"$output_guard_scratch/build.stderr" || status=$?
if [ "$status" -ne 40 ]; then
    cat "$output_guard_scratch/build.stderr" >&2
    printf 'expected build rejection, got %s\n' "$status" >&2
    exit 1
fi
if ! cmp -s src/main.kio "$output_guard_scratch/source.expected"; then
    printf '%s\n' 'build changed its own source input' >&2
    exit 1
fi
cat "$output_guard_scratch/build.stderr" >&2
exit "$status"
