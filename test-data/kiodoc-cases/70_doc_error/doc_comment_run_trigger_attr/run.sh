#!/bin/sh
# A `///` doc-comment fence's attribute vocabulary is `{@}`, `{ignore}`,
# and `check_exit_code=N`. The run-trigger attributes (`stdout`,
# `stderr`, `run_exit_code=N`) are `.md` document forms — they pair a
# snippet with a following output fence — and a doc-comment fence
# carrying one is a runner error, in both the bare-flag and the
# key-value spelling.
#
# Where the bare `@` may sit is a separate rule: it opens the attribute
# list. The sibling `harness_ref_not_first` case covers that.
set -u
"$KIO_BIN" doc check
