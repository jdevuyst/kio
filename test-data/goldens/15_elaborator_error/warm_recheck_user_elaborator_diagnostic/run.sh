#!/bin/sh
# A user elaborator imported across a module boundary raises a
# diagnostic (`__elab_error__`) when its consumer cannot supply the
# requested target. The defining module typechecks and is cached; the
# consuming module fails and is not. A warm recheck must reproduce the
# defining module's real elaborator body (not its un-forced placeholder)
# so the same diagnostic surfaces cold and warm. The WARM_REBUILD_STABLE
# marker drives `warm-recheck-stable.sh`, which asserts that equivalence
# directly; this run only pins the cold diagnostic.
set -u
cd workdir || exit
"$KIO_BIN" check
