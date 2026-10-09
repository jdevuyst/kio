#!/bin/sh
# SUBJECT: Budgeted public product, sum and forall declarations produce compilable Java class filenames.
# CONTRACT: specs/backends/java.md, FFI type mapping and Item naming.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.bounded-public-names.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

python3 - <<'PY'
from pathlib import Path
import re

shapes = Path("out/java/bounded_names/Shapes.java").read_text()
for kind in ("Product", "Sum"):
    names = re.findall(r"\b(KioFacade_V1_" + kind + r"_H[0-9a-f]{16})\b", shapes)
    assert names, kind
    assert all(len(name.encode()) <= 64 for name in names), names
assert re.search(r"\bKioForall_H[0-9a-f]{16}\b", shapes), "long forall identity"
assert "KioForall_Site_M_api__H_short_Binders_K1_B0_A0" in shapes, "readable forall identity"
PY

javac -d "$scratch/classes" out/java/bounded_names/*.java
printf 'bounded public names compile\n'
