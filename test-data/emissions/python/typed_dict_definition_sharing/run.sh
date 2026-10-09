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
    nodes = ast.parse(path.read_text(encoding="utf-8")).body
    typing_bindings = {
        alias.asname or alias.name
        for node in nodes
        if isinstance(node, ast.Import)
        for alias in node.names
        if alias.name == "typing"
    }
    for node in nodes:
        if isinstance(node, ast.ClassDef) and any(
            isinstance(base, ast.Attribute)
            and isinstance(base.value, ast.Name)
            and base.value.id in typing_bindings
            and base.attr == "TypedDict"
            for base in node.bases
        ):
            definitions += 1
if definitions != 1:
    raise SystemExit(f"expected one shared structural dictionary definition, found {definitions}")
print("shared structural dictionary definition")
PY
