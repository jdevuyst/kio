#!/bin/sh
# A syntax error inside a dependency *module body* is a dependency
# error, exactly like a dependency package-file parse failure: module
# bodies parse lazily, so the body is first parsed when materialization
# forces it, and that failure must retag to the dependency tier — exit
# 30 with the `in dependency `<name>`` prefix, keeping the failing
# file's own span for source context (`specs/exit-codes.md` code 30:
# the resolved dependency does not itself collect / parse). Regression:
# the lazy-body force once escaped the retag and surfaced the raw parse
# error as exit 11 with no dependency attribution.
#
# A custom run.sh is justified: the subject is `kio dep fetch`'s
# materialize-time failure, which the standard run.args build path
# never reaches (it expects an already-materialized tree). The
# dependency is a committed local `path` fixture, so the fetch is
# hermetic (no network). SKIP_DEP_MATERIALIZED opts out of the
# dep-canonical check: the tree is deliberately non-materializable.
set -u
cd workdir || exit 1
"$KIO_BIN" dep fetch
