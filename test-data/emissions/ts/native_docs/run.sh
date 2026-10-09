#!/bin/sh
# SUBJECT: TypeScript JSDoc attaches live callable and nominal docs while the declaration skin remains strictly valid.
# CONTRACT: specs/backends/ts.md § Boundary documentation
set -eu
if command -v mise >/dev/null 2>&1; then tsc_path=$(mise which tsc); else tsc_path=$(command -v tsc); fi
scratch=$(mktemp -d "${TMPDIR:?}/emissions.ts-native-docs.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R workdir "$scratch/original"
cp inspect.cjs "$scratch/inspect.cjs"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
tsc --noEmit --strict --target ES2020 out/ts/native_docs.d.ts
node "$scratch/inspect.cjs" out/ts/native_docs.d.ts "$tsc_path"
cp out/ts/native_docs.d.ts "$scratch/documented.d.ts"
node "$scratch/inspect.cjs" --edit-source .
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
node "$scratch/inspect.cjs" out/ts/native_docs.d.ts "$tsc_path" --edited
node "$scratch/inspect.cjs" --strip-source .
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
node "$scratch/inspect.cjs" out/ts/native_docs.d.ts "$tsc_path" "$scratch/documented.d.ts" --absent
cp -R "$scratch/original/." .
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
node "$scratch/inspect.cjs" out/ts/native_docs.d.ts "$tsc_path"
