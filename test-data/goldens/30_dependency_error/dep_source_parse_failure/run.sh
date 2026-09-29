#!/bin/sh
# A dependency whose resolved source does not itself collect/parse is a
# dependency error (exit 30 per specs/exit-codes.md). Here the dependency's
# own package file `broken.pkg.kio` is malformed (missing the `;` after the
# `package` header), so parsing the resolved dependency fails; the failure
# is retagged into the dependency tier and prefixed `in dependency
# `broken``, distinguishing it from a consumer parse error (exit 11).
#
# A custom run.sh is justified: the subject is `kio dep fetch` parsing the
# resolved dependency and failing, which the standard run.args build path
# never reaches. The dependency is a local `path` fixture (hermetic).
# SKIP_DEP_MATERIALIZED opts out of the dep-canonical check: the dependency
# is deliberately non-materializable (its source does not parse).
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
