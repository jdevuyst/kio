#!/bin/sh
# SUBJECT: Generated Python stub methods and nominal classes expose actual native docstrings, with private prose excluded.
# CONTRACT: specs/backends/python.md § Boundary documentation
set -eu
scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-native-docs.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cp -R workdir "$scratch/original"
cp inspect.py "$scratch/inspect.py"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
python3 "$scratch/inspect.py" out/python
cp -R out/python/native_docs "$scratch/documented"
python3 "$scratch/inspect.py" --edit-source .
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
python3 "$scratch/inspect.py" out/python --edited
python3 "$scratch/inspect.py" --strip-source .
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
python3 "$scratch/inspect.py" out/python "$scratch/documented" --absent
cp -R "$scratch/original/." .
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
python3 "$scratch/inspect.py" out/python
