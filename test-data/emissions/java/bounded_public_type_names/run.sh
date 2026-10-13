#!/bin/sh
# SUBJECT: Canonical structural carriers and budgeted dependent forall shapes produce compilable Java class filenames.
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
host = Path("out/java/bounded_names/BoundedNamesHost.java").read_text()
assert re.search(r"public record Product<L, R>\(L left, R right\)", shapes)
assert re.search(r"public sealed interface Sum<L, R>", shapes)

def bounded_name(identity):
    digest = 0xcbf29ce484222325
    for byte in identity.encode():
        digest = ((digest ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
    return f"KioForall_H{digest:016x}"

short = "KioForall_Type_Fn_T0_T0"
assert shapes.count(f"public interface {short} {{") == 1, "one shared short identity"
assert host.count(f"Shapes.{short} p0") == 2, "source-name length does not change identity"
for width in (30, 128):
    identity = "e0;q*f" + "pb0;" * (width - 1) + "b0;b0;"
    name = bounded_name(identity)
    assert len(name.encode()) <= 64 and name.startswith("KioForall_H"), name
    assert f"public interface {name} {{" in shapes, (width, name)
    assert f"api__dependent{width}(Shapes.{name} p0)" in host, (width, name)
PY

javac -d "$scratch/classes" out/java/bounded_names/*.java
printf 'bounded public names compile\n'
