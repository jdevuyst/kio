#!/bin/sh
# `retype` onto a counterpart of a different type-parameter arity is a
# materialize-time dependency error. The dependency's
# `newtype Box[A] : A` and the consumer's `newtype Box[A][B] : A` have
# byte-equal payloads (`A`), so a payload-only congruence check would pass
# — but they are not interchangeable, because every `Box(..)` use site
# applies a different number of type arguments. The arity must be checked
# at materialization and rejected with exit 30 (a clean diagnostic, not a
# downstream `kio check` type error in generated source — `specs/package.md`
# § Retyping, `specs/exit-codes.md` code 30).
#
# A custom run.sh is justified: the subject is `kio dep fetch`'s
# materialize-time congruence check failing, which the standard run.args
# build path never reaches (it expects an already-materialized tree). The
# dependency is a committed local `path` fixture, so the fetch is hermetic
# (no network). SKIP_DEP_MATERIALIZED opts out of the dep-canonical check:
# the tree is deliberately non-materializable.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
