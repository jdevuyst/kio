#!/bin/sh
# Doc-comment {@} snippet carrying `check_exit_code=N`: the snippet is
# designed to fail `kio check`, and declaring the exit code it fails
# with makes that rejection the expected outcome. `kio doc check` exits
# 0 and stays silent — the rejected snippet's own diagnostic belongs to
# the failure path, not to a passing run.
#
# The sibling `snippet_check_exit_code` case pins the same contract for
# the `.md` snippet path. Both pin stderr byte-exactly (`expected.stderr`,
# empty) rather than ignoring it, because the silence *is* the subject.
#
# The fence also covers `check_exit_code=N` composing with the `{@}`
# self-ref, the doc-comment counterpart of `{@NAME check_exit_code=N}`.
set -u
"$KIO_BIN" doc check
