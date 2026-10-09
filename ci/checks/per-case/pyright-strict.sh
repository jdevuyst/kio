#!/bin/sh
# ROUTING: impl
#
# Per-case Python typed-stub type-check.
#
# The Python backend (`kio build python`) emits the runtime `<pkg>.py`
# interpreter module plus a generated `<pkg>/` typed-stub package
# (`specs/backends/python.md` § Output layout, § Typed stub). The golden
# runner runs the `<pkg>.py` (the runtime artifact); this check covers
# the other half of the contract — the stub package must type-check under
# a strict checker, so the typed surface a Python host embeds against is
# well-formed (no implicit anys, valid declaration syntax).
#
# The strict checker is pyright: it reuses the already-pinned node / npm
# toolchain class (no new one) and is what Pylance runs, so a host on VS
# Code checks against the same engine. It is swappable to mypy by design
# — the emitted stubs carry no checker-wide directive, so a mypy
# `--strict` run over the same stub is an equivalent gate; only this
# script's invocation would change.
#
# Paired with the `kio@python` golden impl (the runner runs the `.py`;
# this check type-checks the stub package), per the paired-jobs rule.
#
# Gating: only the `kio@python` impl (`KIO_TARGET == python`). For a case
# that does not build successfully (the error buckets — parse / type /
# totality / …), `kio build python` fails before emitting a stub package,
# exactly as the case run's own build does; there is no stub to
# type-check, so the check skips. Only a case that builds and emits a
# stub package is type-checked.
#
# The check builds a private copy and checks each selected package's
# configured output and effective namespace from the compiler's build plan.
#
# POSIX sh only.

set -eu

if [ "${KIO_TARGET:-}" != "python" ]; then
  exit 0
fi

if [ -z "${KIO_BIN:-}" ]; then
  printf 'pyright-strict: KIO_BIN not set\n' >&2
  exit 2
fi

# Locate pyright. It is provisioned as a mise npm tool (mise.toml pins
# `npm:pyright`), so it is on PATH the same way `tsc` is; `npx
# --no-install pyright` is the fallback for a local checkout with a
# project-local install. A missing pyright is a setup error, not a silent
# skip — the type-check is a contract.
if command -v mise >/dev/null 2>&1 && PYRIGHT=$(cd "$(dirname "$0")" && mise which pyright 2>/dev/null) && [ -n "$PYRIGHT" ]; then
  # Resolve the mise shim to its real binary here, from the script's own
  # in-repo directory (a generated case's cwd sits outside the repo,
  # where mise has no config): a shim re-resolves its version from
  # the cwd's mise config on every invocation, and the check below runs
  # from a scratch directory outside any config, where the bare shim
  # fails with "No version is set".
  :
elif command -v pyright >/dev/null 2>&1; then
  PYRIGHT="pyright"
elif command -v npx >/dev/null 2>&1 && npx --no-install pyright --version >/dev/null 2>&1; then
  PYRIGHT="npx --no-install pyright"
else
  # shellcheck disable=SC2016 # backticks are literal text
  printf 'pyright-strict: no `pyright` on PATH (install it; mise.toml pins npm:pyright@1.1.411)\n' >&2
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
  printf 'pyright-strict: node is required to read the output plan\n' >&2
  exit 2
fi

# Build the python target in a private scratch copy of `workdir`, never
# in `workdir` itself: `kio build python` writes the package's on-disk
# caches (and the emitted output) under the package's `out/` directory,
# and polluting the real `workdir` would corrupt a cache-sensitive case
# run that follows this check. The scratch copy is removed on exit.
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
  printf 'pyright-strict: successful build has no valid output plan:\n' >&2
  sed 's/^/    /' "$scratch/plan.log" >&2
  exit 1
fi
"$NODE" "$helper" prepare "$copied_workdir" "$plan" python || exit 1

# A non-zero status means the case does not build (an error-bucket
# fixture); there is no stub package to type-check, so skip — the case run
# itself asserts the expected build error.
if ! "$KIO_BIN" build "$KIO_TARGET" >/dev/null 2>&1; then
  exit 0
fi

case "$PYRIGHT" in
  'npx --no-install pyright') set -- npx --no-install pyright ;;
  *) set -- "$PYRIGHT" ;;
esac
"$NODE" "$helper" check "$copied_workdir" "$plan" python "$@"
