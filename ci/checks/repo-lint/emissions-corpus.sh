#!/bin/sh
#
# Cheap structural gate for test-data/emissions/. The corpus orchestrator calls
# the same shared validator before building; this standalone lint keeps the
# entire corpus gated even when a selected implementation cohort excludes some
# backend buckets from execution.
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

# shellcheck disable=SC1091
. "$REPO_ROOT/ci/checks/orchestrators/lib/emissions-contract.sh"

EMISSIONS_CONTRACT_PREFIX=emissions-corpus
export EMISSIONS_CONTRACT_PREFIX
CORPUS_ROOT=${KIO_EMISSIONS_CORPUS_ROOT:-$REPO_ROOT/test-data/emissions}

if emissions_validate_corpus "$CORPUS_ROOT"; then
  printf 'emissions-corpus: OK\n'
else
  exit 1
fi
