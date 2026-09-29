#!/bin/sh
# The emissions corpus owns host compilation and artifact inspection in each
# custom run.sh. Reaching a generic runner means a case escaped that boundary.

printf '%s\n' \
  'error: emissions cases must inspect or host emitted artifacts from run.sh; the generic test runner must not be invoked' >&2
exit 1
