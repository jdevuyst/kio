#!/bin/sh
# SUBJECT: Native boundary documentation preserves live prose and retained deprecation without changing executable artifacts.
# CONTRACT: specs/backends/java.md, Doc comments.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.native-docs.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
python3 - <<'PY'
from pathlib import Path
path = Path("testapi/main.kio")
source = path.read_bytes()
needle = b"/// EXPORT-DOC main."
assert source.count(needle) == 1
path.write_bytes(source.replace(needle, needle + b" big.Int.\rCR-DOC remains commentary."))
PY
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

javadoc -quiet -Xdoclint:all,-missing -d "$scratch/docs" out/java/native_docs/*.java >"$scratch/javadoc.log" 2>&1 || {
  cat "$scratch/javadoc.log" >&2
  exit 1
}

python3 - "$scratch" <<'PY'
from pathlib import Path
import re
import sys
scratch = Path(sys.argv[1])
documentation = "\\n".join(p.read_text() for p in (scratch / "docs").rglob("*.html"))
for marker in ["EXPORT-DOC","FIRST-SAME-DOC","SECOND-SAME-DOC","HOST-FN-DOC","HOST-GENERIC-DOC","OPAQUE-DOC","GENERIC-DOC","EXISTENTIAL-DOC","CR-DOC","NEWTYPE-DOC"]:
    assert re.search(r"(?<![A-Z-])" + re.escape(marker) + r"(?![A-Z-])", documentation), marker
for marker in ("PRIVATE-DOC", "MODULE-DOC", "UNBRIDGED-DOC"):
    assert marker not in documentation, marker
assert "HOST-TYPE-DOC" not in documentation
def method(page, selector):
    html = (scratch / "docs/native_docs" / page).read_text()
    match = re.search(r'<section class="detail" id="' + re.escape(selector) + r'">(.*?)</section>', html, re.S)
    assert match, (page, selector)
    return match.group(1)
for module, marker, other in (("main", "FIRST-SAME-DOC", "SECOND-SAME-DOC"),
                              ("other", "SECOND-SAME-DOC", "FIRST-SAME-DOC")):
    selected = method("NativeDocs.Ns_M_testapi__M_" + module + ".html", "same()")
    assert marker in selected and other not in selected, module
opaque = (scratch / "docs/native_docs/Shapes.KioNewtype_testapi__Opaque.html").read_text()
assert "OPAQUE-DOC" in opaque and "GENERIC-DOC" not in opaque
for carrier, marker in (("KioHostType_testapi__Box", "HOST-GENERIC-DOC"),
                        ("KioHostTypeMk_testapi__Box", "HOST-GENERIC-DOC"),
                        ("KioNewtype_testapi__Token", "NEWTYPE-DOC"),
                        ("KioNewtypeMk_testapi__Wrapper", "GENERIC-DOC"),
                        ("KioNewtype_testapi__Packed", "EXISTENTIAL-DOC")):
    page = (scratch / "docs/native_docs" / ("Shapes." + carrier + ".html")).read_text()
    selected = re.search(r'<section class="class-description" id="class-description">(.*?)</section>', page, re.S)
    assert selected and marker in selected.group(1), carrier
for carrier, note in (("KioHostType_testapi_sretired__Gone", "Storage operations for this exact host declaration."),
                      ("KioHostTypeMk_testapi_sretired__Gone", "Exact constructor marker for `testapi/retired.Gone` (arity 1; kinds: *).")):
    page = (scratch / "docs/native_docs" / ("Shapes." + carrier + ".html")).read_text()
    selected = re.search(r'<section class="class-description" id="class-description">(.*?)</section>', page, re.S)
    assert selected, carrier
    selected = selected.group(1)
    assert note in selected, (carrier, selected)
    assert "@Deprecated" in selected, (carrier, selected)
    assert "Retained only for source compatibility after removal at contract v2." in selected, (carrier, selected)
PY

cp -R . "$scratch/plain"
python3 - "$scratch/plain" <<'PY'
from pathlib import Path
import sys
for path in Path(sys.argv[1]).rglob("*.kio"):
    path.write_bytes(b"\n".join(line for line in path.read_bytes().split(b"\n")
                               if not line.lstrip().startswith(b"///")))
PY
(
  cd "$scratch/plain"
  "${KIO_BIN:?}" build "${KIO_TARGET:?}"
)
python3 - "$scratch/plain/out/java" <<'PY'
from pathlib import Path
import re
import sys
def tree(root):
    return {p.relative_to(root): p.read_text() for p in root.rglob("*.java")}
def without_documentation(source):
    source = re.sub(r"/\*\*.*?\*/", "", source, flags=re.S)
    return "\n".join(line for line in source.splitlines() if line.strip())
documented = tree(Path("out/java"))
plain = tree(Path(sys.argv[1]))
assert documented and documented.keys() == plain.keys()
for path in documented:
    assert not re.search(r"[A-Z]+(?:-[A-Z]+)*-DOC", plain[path]), path
    assert without_documentation(documented[path]) == without_documentation(plain[path]), path
PY
printf 'native java docs verified\n'
