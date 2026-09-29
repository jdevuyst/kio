#!/bin/sh
# A dependency `source { path ... }` must name an existing `*.pkg.kio`
# package file, resolved against the consumer package root. Here the path
# resolves to nothing on disk, so materialization fails with a dependency
# error (exit 30 per specs/exit-codes.md; specs/package.md § Dependency
# files).
#
# A custom run.sh is justified: the subject is `kio dep fetch`'s
# resolution failing, which the standard run.args build path never reaches.
# The dependency is a local `path` fixture, so the fetch is hermetic (no
# network). SKIP_DEP_MATERIALIZED opts out of the dep-canonical check: the
# dependency is deliberately non-materializable (its source does not exist).
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
