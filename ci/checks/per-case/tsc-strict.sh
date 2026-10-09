#!/bin/sh
# ROUTING: impl
#
# Per-case TypeScript skin type-check.
#
# The TypeScript backend (`kio build ts`) emits the JS backend's
# `<pkg>.js` byte-identical plus a generated `<pkg>.d.ts` typed-skin
# sidecar (`specs/backends/ts.md` § Output layout). The golden runner
# runs the `<pkg>.js` (the runtime artifact); this check covers the
# other half of the contract — the `<pkg>.d.ts` must type-check under
# `tsc --strict --noEmit`, so the skin a TypeScript host calls against
# is well-formed (no implicit `any`, valid declaration syntax).
#
# Paired with the `kio@ts` golden impl (the runner runs the `.js`; this
# check type-checks the `.d.ts`), per the paired-jobs rule.
#
# Gating: only the `kio@ts` impl (`KIO_TARGET == ts`). For a case that
# does not build successfully (the error buckets — parse / type /
# totality / … — whose subject is the build error itself), `kio build
# ts` fails before emitting a `.d.ts`, exactly as the case run's own
# build does; there is no skin to type-check, so the check skips. Every
# successful build must emit its declaration sidecar and type-check it.
#
# The check builds a private copy and checks each selected package's
# configured output and effective namespace from the compiler's build plan.
#
# POSIX sh only.

set -eu

if [ "${KIO_TARGET:-}" != "ts" ]; then
  exit 0
fi

if [ -z "${KIO_BIN:-}" ]; then
  printf 'tsc-strict: KIO_BIN not set\n' >&2
  exit 2
fi

# Locate tsc. The dev container installs `typescript` globally, so
# `tsc` is on PATH; `npx --no-install tsc` is the fallback for a local
# checkout with a project-local install. A missing tsc is a setup error,
# not a silent skip — the type-check is a contract.
if command -v mise >/dev/null 2>&1 && TSC=$(cd "$(dirname "$0")" && mise which tsc 2>/dev/null) && [ -n "$TSC" ]; then
  # Resolve the mise shim to its real binary here, from the script's own
  # in-repo directory (a generated case's cwd sits outside the repo,
  # where mise has no config): a shim re-resolves its version from
  # the cwd's mise config on every invocation, and the check below runs
  # from a scratch directory outside any config, where the bare shim
  # fails with "No version is set".
  :
elif command -v tsc >/dev/null 2>&1; then
  TSC="tsc"
elif command -v npx >/dev/null 2>&1 && npx --no-install tsc --version >/dev/null 2>&1; then
  TSC="npx --no-install tsc"
else
  # shellcheck disable=SC2016 # backticks are literal text
  printf 'tsc-strict: no `tsc` on PATH (install typescript; the dev container pins typescript@6.0.3)\n' >&2
  exit 2
fi

# Cases run from their case directory; the package lives in `workdir`.
# A case with no `workdir` has nothing to build.
if [ ! -d workdir ]; then
  exit 0
fi

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
helper=$script_dir/../../infra/typed-output-check.mjs
# Resolve the pinned runtime before entering a scratch directory without mise config.
if command -v mise >/dev/null 2>&1 && NODE=$(cd "$script_dir" && mise which node 2>/dev/null) && [ -n "$NODE" ]; then
  :
elif command -v node >/dev/null 2>&1; then
  NODE=$(command -v node)
else
  printf 'tsc-strict: node is required to read the output plan\n' >&2
  exit 2
fi

# Build the TS target in a private scratch copy of `workdir`, never in
# `workdir` itself: `kio build ts` writes the package's on-disk caches
# (and the emitted output) under the package's `out/` directory, and
# polluting the real `workdir` would corrupt a cache-sensitive case run
# that follows this check (a cold-cache golden would see a warm cache).
# The scratch copy is removed on exit.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT INT TERM HUP
copied_workdir=$scratch/work
mkdir "$copied_workdir"
cp -R workdir/. "$copied_workdir/" || exit 2
cd "$copied_workdir" || exit 2

plan=$scratch/typed-output-plan.json
if ! "$KIO_BIN" debug build-output-plan "$KIO_TARGET" >"$plan" 2>"$scratch/plan.log"; then
  if ! "$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>&1; then
    exit 0
  fi
  printf 'tsc-strict: successful build has no valid output plan:\n' >&2
  sed 's/^/    /' "$scratch/plan.log" >&2
  exit 1
fi
"$NODE" "$helper" prepare "$copied_workdir" "$plan" ts || exit 1

# A non-zero status means the case does not build (an error-bucket
# fixture); there is no `.d.ts` to type-check, so skip — the case run
# itself asserts the expected build error.
if ! "$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>&1; then
  exit 0
fi

case "$TSC" in
  'npx --no-install tsc') set -- npx --no-install tsc ;;
  *) set -- "$TSC" ;;
esac
"$NODE" "$helper" check "$copied_workdir" "$plan" ts "$@" --strict --noEmit
