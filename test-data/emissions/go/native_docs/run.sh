#!/bin/sh
# SUBJECT: Live boundary documentation is attached to native declarations and does not change executable artifacts.
# CONTRACT: specs/backends/go.md, Doc comments.
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
source = source.replace(needle, needle + b" big.Int.\rCR-DOC remains commentary.\x00 NUL-DOC \xef\xbb\xbf BOM-DOC")
source = source.replace(b"`literal`", b"`literal\x00\xef\xbb\xbf`")
source = source.replace(b"[`literal_code`]", b"[`literal_code`]\x00\xef\xbb\xbf")
path.write_bytes(source)
PY
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

(
  cd out/go
  GO111MODULE=off go doc -all -u . >"$scratch/native-docs.txt"
)

python3 - "$scratch" <<'PY'
from pathlib import Path
import os
import re
import subprocess
import sys
scratch = Path(sys.argv[1])
documentation = (scratch / "native-docs.txt").read_text()
def has_marker(marker):
    return re.search(r"(?<![A-Z-])" + re.escape(marker) + r"(?![A-Z-])", documentation)
for marker in ["EXPORT-DOC","FIRST-SAME-DOC","SECOND-SAME-DOC","HOST-FN-DOC","HOST-GENERIC-DOC","OPAQUE-DOC","CR-DOC","NUL-DOC","BOM-DOC"]:
    assert has_marker(marker), marker
for marker in ("PRIVATE-DOC", "MODULE-DOC", "UNBRIDGED-DOC"):
    assert marker not in documentation, marker
assert "HOST-TYPE-DOC" not in documentation
assert "NEWTYPE-DOC" not in documentation
assert not has_marker("GENERIC-DOC")
assert not has_marker("EXISTENTIAL-DOC")
def doc(symbol):
    return subprocess.check_output(["go", "doc", "-u", ".", symbol], cwd="out/go",
                                   env={**os.environ, "GO111MODULE": "off"}, text=True)
def field_type(owner, field):
    match = re.search(r"(?m)^\s*" + re.escape(field) + r"\s+([A-Za-z0-9_]+)", doc(owner))
    assert match, (owner, field)
    return match.group(1)
testapi = field_type("NativeDocs", "Testapi")
for field, marker, other in (("KioModule_main", "FIRST-SAME-DOC", "SECOND-SAME-DOC"),
                            ("KioModule_other", "SECOND-SAME-DOC", "FIRST-SAME-DOC")):
    selected = doc(field_type(testapi, field) + ".Same")
    assert marker in selected and other not in selected, (field, selected)
assert "HOST-FN-DOC" in doc("NativeDocsHost.Testapi_io__print")
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
python3 - "$scratch/plain/out/go" <<'PY'
from pathlib import Path
import re
import sys
def tree(root):
    return {p.relative_to(root): p.read_text() for p in root.rglob("*.go")}
def without_documentation(source):
    return "\n".join(line for line in source.splitlines()
                     if line.strip() and not line.lstrip().startswith("//"))
documented = tree(Path("out/go"))
plain = tree(Path(sys.argv[1]))
assert documented and documented.keys() == plain.keys()
for path in documented:
    assert not re.search(r"[A-Z]+(?:-[A-Z]+)*-DOC", plain[path]), path
    assert without_documentation(documented[path]) == without_documentation(plain[path]), path
assert '"math/big"' not in documented[Path("pkg.go")]
PY
printf 'native go docs verified\n'
