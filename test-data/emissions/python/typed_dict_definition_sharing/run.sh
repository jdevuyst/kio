#!/bin/sh
# SUBJECT: Repeated exact structural dictionaries share one checker definition.
# CONTRACT: A portable proxy for measured strict-checker definition memory.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.typed-dict-sharing.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

python3 - <<'PY'
import ast
from pathlib import Path

definitions = 0
for path in Path("out/python/sharing").glob("*.pyi"):
    for node in ast.parse(path.read_text(encoding="utf-8")).body:
        if isinstance(node, ast.ClassDef) and any(
            isinstance(base, ast.Attribute)
            and isinstance(base.value, ast.Name)
            and base.value.id == "typing"
            and base.attr == "TypedDict"
            for base in node.bases
        ):
            definitions += 1
if definitions != 1:
    raise SystemExit(f"expected one shared structural dictionary definition, found {definitions}")
print("shared structural dictionary definition")
PY
