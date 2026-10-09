#!/bin/sh
# SUBJECT: Distinct Java facade identities produce distinct classfile paths under case-insensitive filesystem comparison.
# CONTRACT: specs/backends/README.md, Source-derived public names; specs/backends/java.md, Item naming.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.java-classfile-names.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"
mkdir "$scratch/java-tmp"
javac -J-Djava.io.tmpdir="$scratch/java-tmp" -d "$scratch/classes" out/java/portable_names/*.java

python3 - "$scratch/classes" <<'PY'
from pathlib import Path
import sys

root = Path(sys.argv[1])
classes = sorted(root.rglob("*.class"))
assert classes, "native compiler produced no classfiles"
physical = {}
for path in classes:
    relative = path.relative_to(root).as_posix()
    key = relative.casefold()
    assert key not in physical, (physical.get(key), relative)
    physical[key] = relative
    if path.name.startswith("Shapes$"):
        assert len(path.name.encode("ascii")) <= 240, path.name
    if path.name.startswith(("PortableNames$Ns_", "PortableNames$NtH_")):
        identifier = path.stem.split("$")[1]
        assert len(identifier.encode("ascii")) <= 64, identifier
expected = {
    "Shapes$KioModule_api$Types.class",
    "Shapes$KioModule_api$KioModule_types$Token.class",
    "Shapes$KioModule_api$Foo_bar.class",
    "Shapes$KioModule_api$Foobar.class",
    "Shapes$KioModule_api$KioModule_foo_ubar$Token.class",
    "Shapes$KioModule_api$KioModule_foobar$Token.class",
    "Shapes$KioModule_api$KioType_Kiomodule_utypes.class",
    "Shapes$KioModule_api$KioType_Kiotype_ushapes.class",
    "Shapes$KioModule_api$KioType_Kionominal_uhost.class",
    "PortableNames$Ns_M_api__M_foo_ubar.class",
    "PortableNames$Ns_M_api__M_foobar.class",
    "PortableNames$NtH_M_api__T_Foo_ubar.class",
    "PortableNames$NtH_M_api__T_Foobar.class",
}
actual = {path.name for path in classes}
def digest(identity):
    value = 0xcbf29ce484222325
    for byte in identity.encode("ascii"):
        value = ((value ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
    return f"{value:016x}"

long_module = "_".join(["a"] * 80)
exact_module = long_module.replace("_", "_u")
expected.add("PortableNames$Ns_H" + digest("Ns_M_" + exact_module) + ".class")
expected.add("PortableNames$NtH_H" + digest("NtH_M_" + exact_module + "__T_Token") + ".class")
expected.add("Shapes$KioNominal_Newtype_H" + digest("Newtype:" + long_module + "/Token") + ".class")
assert expected <= actual, ("missing public classfiles", sorted(expected - actual))
PY
printf 'Java classfile identities remain distinct under case folding\n'
