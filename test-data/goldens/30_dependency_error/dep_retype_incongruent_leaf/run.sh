#!/bin/sh
# A per-type `retype Wrap` whose payload references a sibling **non-host**
# nominal `Inner` that differs across the two modules is a materialize-time
# dependency error. `specs/package.md` § Retyping sanctions
# by-name leaf matching only for **host** types; a non-host nominal leaf
# must match by identity. Here each side's `Inner` is a
# distinct nominal — `widget.store.Inner` (a two-constructor sum) vs
# `core.store.Inner` (unit) — and `Inner` is not itself retyped, so the
# payloads are not congruent and the remap is rejected with exit 30 (a
# clean diagnostic, not a downstream `kio check` failure in generated
# source — `specs/exit-codes.md` code 30).
#
# A custom run.sh is justified: the subject is `kio dep fetch`'s
# materialize-time congruence check failing, which the standard run.args
# build path never reaches. The dependency is a committed local `path`
# fixture, so the fetch is hermetic. SKIP_DEP_MATERIALIZED opts out of the
# dep-canonical check (the tree is deliberately non-materializable).
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
