#!/bin/sh
# End-to-end `kio sig` lifecycle, asserting each `8x` status exit code
# at the step that produces it. The case exits 0 overall; the load-
# bearing assertions are the intermediate `kio sig status` codes
# (80 / 81 / 82 / 0) and the stage / commit transitions between them.
#
# The flow *mutates* the package (rewrites `api.kio`, writes
# `app.sig.kio`), so it runs in a private scratch copy of `workdir`
# rather than in `workdir` itself — the harness may run this case on
# several impls concurrently against one shared `workdir`, and in-place
# mutation would let them stomp each other.
#
# `kio sig` is a surface-only command (kio-prime rejects it), so the
# case carries IS_KIO_PRIME (its module sources are Kio'-shaped) plus
# SKIP_KIO_PRIME_RUN to opt out of the kio-prime run while keeping the
# source biconditional honest.
set -u

scratch=$(mktemp -d) || { printf 'cannot make scratch dir\n' >&2; exit 1; }
trap 'rm -rf "$scratch"' EXIT
cp workdir/app.pkg.kio workdir/api.kio "$scratch/" || exit 1
cd "$scratch" || exit 1

# Assert that `$@` exits with the expected status (first arg).
expect_exit() {
  want=$1
  shift
  "$@" >/dev/null 2>&1
  got=$?
  if [ "$got" -ne "$want" ]; then
    # shellcheck disable=SC2016
    printf 'expected exit %s from `%s`, got %s\n' "$want" "$*" "$got" >&2
    exit 1
  fi
}

# Fresh package: the export `api.serve` is an unrecorded compatible
# addition against the empty sealed baseline (no host items here, so no
# breaking env adds) — status 81 (stale-but-compatible).
expect_exit 81 "$KIO_BIN" sig status

# Record the compatible delta into the draft (no --force needed).
expect_exit 0 "$KIO_BIN" sig stage

# After recording, the draft matches the live surface; nothing is sealed
# yet and there is no recorded break — status 0 (clean: a compatible
# recorded delta is not pending).
expect_exit 0 "$KIO_BIN" sig status

# Seal v(1) -> open v(2).
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status

# Introduce a breaking change: remove the sealed export `serve`.
printf 'module api;\n\npub fn other() -> . { () }\n' > api.kio

# The break is unrecorded -> status 80 (incompatibility).
expect_exit 80 "$KIO_BIN" sig status

# `kio sig stage` refuses to record a break (exit 40, build-error tier).
expect_exit 40 "$KIO_BIN" sig stage

# `--force` records the break into the draft.
expect_exit 0 "$KIO_BIN" sig stage --force

# Now the break IS recorded but not sealed -> status 82
# (unsealed-break-pending).
expect_exit 82 "$KIO_BIN" sig status

# `commit` seals the recorded break; status returns to clean.
expect_exit 0 "$KIO_BIN" sig commit
expect_exit 0 "$KIO_BIN" sig status

# `kio sig log` pretty-prints the changelog and exits 0.
expect_exit 0 "$KIO_BIN" sig log

# The changelog now records the v(2) breaking removal of `serve`.
"$KIO_BIN" sig log | grep -q 'serve' || {
  # shellcheck disable=SC2016
  printf 'changelog should still record the removed export `serve`\n' >&2
  exit 1
}
