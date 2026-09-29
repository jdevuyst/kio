#!/bin/sh
#
# The dyn_load_prime integration golden: a host that vendors the dyn_load_prime
# package and loads a guest's emitted Prime image at runtime, end to end
# on a real target.
#
# dyn_load_prime is a library a host vendors. This run.sh performs that
# vendoring: it assembles
# the live test-data/poc/dyn_load_prime/workdir/ source (the single source
# of truth — no committed fork) alongside this golden's host module
# (workdir/testapi/main.kio, a `testapi/main` module that loads a guest
# image and calls its exports) and a manifest, builds the host package to
# the impl's target, and runs it through the impl's runner with the
# testapi-dyn-load protocol (the host env dyn_load_prime requires). Its stdout is
# diffed against expected.stdout by the harness.
#
# The runner is invoked exactly as the standard path would
# (`$KIO_RUNNER --protocol testapi-dyn-load out/$KIO_TARGET`); a custom
# run.sh is used only to assemble the vendored package, which the standard
# run.args path (building the case's own workdir) cannot express.
#
# The manifest declares every runtime target. The dyn_load_prime loader's
# `instantiate` packs the loaded surface behind an existential newtype, so
# this golden exercises each target's existential boundary and the complete
# host surface required by the `testapi-dyn-load` protocol.
#
# Environment (set by ci/run-tests.sh): KIO_BIN, KIO_RUNNER, KIO_TARGET.
#
# POSIX sh only.

set -eu

CASE_DIR=$(pwd)
REPO_ROOT=$(CDPATH='' cd -- "$CASE_DIR" && while [ ! -d test-data ] && [ "$(pwd)" != / ]; do cd ..; done && pwd)
PKG_SRC="$REPO_ROOT/test-data/poc/dyn_load_prime/workdir"
if [ ! -d "$PKG_SRC" ]; then
  printf 'exec_dyn_load_integration: cannot locate test-data/poc/dyn_load_prime/workdir from %s\n' "$CASE_DIR" >&2
  exit 2
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM HUP

# Vendor the live dyn_load_prime package (every module + its testapi host
# surface), minus its own manifest. The package's own worked example lives
# in `testapi/main` and is replaced below by this golden's host. `cp`
# follows the shared-support-module symlinks (match / spine_elaborators /
# elaborator_util), so the vendored copy is self-contained.
for f in "$PKG_SRC"/*.kio; do
  base=$(basename "$f")
  case "$base" in
    dyn_load_prime.pkg.kio) continue ;;
  esac
  cp "$f" "$work/$base"
done
mkdir -p "$work/testapi"
cp "$PKG_SRC"/testapi/*.kio "$work/testapi/"
cp -R "$PKG_SRC/list" "$work/list"
cp -R "$PKG_SRC/loader" "$work/loader"
cp -R "$PKG_SRC/elab" "$work/elab"

# Drop in this golden's host module and manifest (the host's `testapi/main`
# replaces the package's worked-example entry; the manifest's bridge
# exposes it plus the vendored dyn_load_prime modules).
cp "$CASE_DIR/workdir/testapi/main.kio" "$work/testapi/main.kio"
cp "$CASE_DIR/workdir/integration.pkg.kio" "$work/integration.pkg.kio"

( cd "$work" && "$KIO_BIN" build "$KIO_TARGET" ) >&2

"$KIO_RUNNER" --protocol testapi-dyn-load "$work/out/$KIO_TARGET"
