#!/bin/sh
# SUBJECT: Native JSDoc documents live host requirements and public function/type properties without publishing private docs.
# CONTRACT: specs/backends/js.md § Boundary documentation
set -eu
if command -v mise >/dev/null 2>&1; then
  tsc_path=$(MISE_ENABLE_TOOLS="${MISE_ENABLE_TOOLS:+$MISE_ENABLE_TOOLS,}npm:typescript" mise which tsc)
else
  tsc_path=$(command -v tsc)
fi
scratch=$(mktemp -d "${TMPDIR:?}/emissions.js-native-docs.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R workdir "$scratch/original"
cp inspect.cjs "$scratch/inspect.cjs"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
node --check out/js/native_docs.js
node "$scratch/inspect.cjs" out/js/native_docs.js "$tsc_path"
cp out/js/native_docs.js "$scratch/documented.js"
node "$scratch/inspect.cjs" --edit-source .
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
node "$scratch/inspect.cjs" out/js/native_docs.js "$tsc_path" --edited
node "$scratch/inspect.cjs" --strip-source .
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
node "$scratch/inspect.cjs" out/js/native_docs.js "$tsc_path" "$scratch/documented.js" --absent
cp -R "$scratch/original/." .
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
node "$scratch/inspect.cjs" out/js/native_docs.js "$tsc_path"
