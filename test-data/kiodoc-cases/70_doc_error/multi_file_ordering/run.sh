#!/bin/sh
# Multi-file error ordering: a directory of markdown files each
# carrying multiple kiodoc-level violations. The driver's
# post-parallel sort must emit the diagnostics in (file_path,
# span.start) order regardless of how many rayon workers run.
#
# Every snippet here typechecks cleanly and only contradicts its
# declared `check_exit_code`, so none contributes a nested `kio check`
# diagnostic and no scratch path reaches the output. The rendered
# messages are therefore fully deterministic, and the rare
# byte-identical `expected.stderr` golden locks the ordering contract —
# a `.grep` policy matches the same four lines in any order, so it
# could not have caught workers interleaving their output.
set -u
"$KIO_BIN" doc check
