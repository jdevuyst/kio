#!/bin/sh
#
# Lint every markdown file in the project's coverage glob:
# root *.md, ai/**/*.md, specs/**/*.md, and docs/**/*.md. Uses
# markdownlint-cli2; config lives in .markdownlint.json at the repo root.
#
# Requires `markdownlint-cli2` on PATH. Install the pinned tool with:
#   mise install --locked npm:markdownlint-cli2
#
# POSIX sh only.

set -eu

SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH='' cd -- "$SCRIPT_DIR/../../.." && pwd)

cd "$REPO_ROOT"

markdownlint-cli2 '*.md' 'ai/**/*.md' 'specs/**/*.md' 'docs/**/*.md'
