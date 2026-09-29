#!/bin/sh
# Cold / warm determinism: two consecutive `kio doc` runs against
# the same input must produce byte-identical output, with the
# second run hitting every cache entry. The build block's
# `cache "out/.kio-cache/"` enables the on-disk cache; the
# runner clears it first so the first run is unambiguously cold.
#
# The cache probe would surface per-snippet hit/miss lines to stderr,
# but this case asserts on the validator's own stderr (empty for a
# fully-passing run), so the probe stays off here. The cache's
# determinism is asserted via the diff
# between the two captures.
set -eu

# Ensure cold state.
rm -rf out/.kio-cache

cold_stdout=$(mktemp)
cold_stderr=$(mktemp)
warm_stdout=$(mktemp)
warm_stderr=$(mktemp)
cleanup() {
  rm -f "$cold_stdout" "$cold_stderr" "$warm_stdout" "$warm_stderr"
  rm -rf out
}
trap cleanup EXIT

# Cold run — every snippet misses, every snippet writes.
"$KIO_BIN" doc check >"$cold_stdout" 2>"$cold_stderr"
cold_exit=$?

# Warm run — every snippet must hit.
"$KIO_BIN" doc check >"$warm_stdout" 2>"$warm_stderr"
warm_exit=$?

# Both runs must succeed.
if [ "$cold_exit" -ne 0 ]; then
  printf 'cold run failed (exit %d):\n' "$cold_exit" >&2
  cat "$cold_stderr" >&2
  exit "$cold_exit"
fi
if [ "$warm_exit" -ne 0 ]; then
  printf 'warm run failed (exit %d):\n' "$warm_exit" >&2
  cat "$warm_stderr" >&2
  exit "$warm_exit"
fi

# Determinism: warm == cold, byte for byte.
if ! cmp -s "$cold_stdout" "$warm_stdout"; then
  printf 'stdout differs between cold and warm runs:\n' >&2
  diff -u "$cold_stdout" "$warm_stdout" >&2 || true
  exit 1
fi
if ! cmp -s "$cold_stderr" "$warm_stderr"; then
  printf 'stderr differs between cold and warm runs:\n' >&2
  diff -u "$cold_stderr" "$warm_stderr" >&2 || true
  exit 1
fi

# Surface the cold stdout / stderr as the case's own output for
# the run-tests diff harness. Empty in the success case.
cat "$cold_stdout"
cat "$cold_stderr" >&2
