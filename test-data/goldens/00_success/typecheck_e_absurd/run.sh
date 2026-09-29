#!/bin/sh
# Subject: § 7.7 Bottom and `__absurd__` (specs/formal/elaboration.md).
#
# `__absurd__`'s scheme is `∀α. (!) → α`. The single binder `[α]`
# is reachable only through the return type, so an E-App call
# with the type-arg elided pins `[α]` from the surrounding
# expected return type.
#
# Subject-of-subject: the `__absurd__` intrinsic + its
# inference-via-return-type path. typecheck_return_type_inference
# names this case as its first cell but spreads its subject across
# three intrinsics; this golden's subject IS `__absurd__`.
set -u
cd workdir || exit
"$KIO_BIN" check
