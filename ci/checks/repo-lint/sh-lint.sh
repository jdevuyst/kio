#!/bin/sh
#
# Run shellcheck over every shell script under
# .devcontainer/, ci/, reports/, and test-data/.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

find .devcontainer ci reports test-data -name '*.sh' -type f -exec shellcheck --shell=sh {} +
