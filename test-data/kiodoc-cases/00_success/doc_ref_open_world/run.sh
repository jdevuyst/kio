#!/bin/sh
# Open-world safety: adding a new declaration to a module does not
# change how a pre-existing [`name`] intra-doc reference resolves.
#
# This tests that resolution is structurally deterministic — the
# [`identity`] ref in input.kio still resolves after adding `extra`
# to the same module. The module has two top-level fns; the ref is
# to `identity` which is listed before `extra` and after `extra`.
# Adding `extra` must not cause [`identity`] to become ambiguous or
# unresolved.
set -u
"$KIO_BIN" doc check
