#!/bin/sh
# A dependency's local name must not equal the first path segment of any
# of the consumer package's own modules, so that `use <local>/…;` resolves
# unambiguously to the dependency. Here the dependency local name `widget`
# collides with the consumer's own root module `widget` (workdir/widget.kio,
# `module widget;`), which sits beside the dependency's materialization
# directory — a dependency error (exit 30 per specs/exit-codes.md;
# specs/package.md § Dependency files, Open-world collision rule).
#
# A custom run.sh is justified: the subject is `kio dep fetch`'s collision
# check, which the standard run.args build path never reaches. The
# dependency is a committed local `path` fixture, so the fetch is hermetic.
# SKIP_DEP_MATERIALIZED opts out of the dep-canonical check: the dependency
# is deliberately non-materializable (the collision blocks it).
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
