#!/bin/sh
# SUBJECT: Recursive-group effect bodies stay single-copy in emitted python source.
# CONTRACT: Measured recursive-dispatcher body sharing.
set -eu

scratch=$(mktemp -d "${TMPDIR:?}/emissions.python-rec-group.XXXXXX")
trap 'rm -rf "$scratch"' 0 HUP INT TERM
cp -R workdir "$scratch/workdir"
cd "$scratch/workdir"
"${KIO_BIN:?}" build "${KIO_TARGET:?}"

find "out/$KIO_TARGET" -type f -name '*.py' -exec cat {} + > "$scratch/emitted"
python3 - "$scratch/emitted" <<'PY'
import ast
import json
from pathlib import Path
import sys

tree = ast.parse(Path(sys.argv[1]).read_text(encoding="utf-8"))
assignments = [node for node in ast.walk(tree) if isinstance(node, ast.Assign)
               and any(isinstance(target, ast.Name) and target.id == "_KIO_IR"
                       for target in node.targets)]
assert len(assignments) == 1, "exactly one IR assignment"
assignment = assignments[0]
assert len(assignment.targets) == 1
call = assignment.value
assert isinstance(call, ast.Call) and isinstance(call.func, ast.Attribute)
assert isinstance(call.func.value, ast.Name) and call.func.value.id == "json"
assert call.func.attr == "loads" and len(call.args) == 1 and not call.keywords
argument = call.args[0]
# Python's parser joins adjacent literals and decodes their quoting without executing them.
assert isinstance(argument, ast.Constant) and isinstance(argument.value, str)
counts = dict.fromkeys([
    "shared-first-effect", "shared-second-before", "shared-second-after",
    "shared-third-effect", "shared-fourth-effect",
], 0)
pending = [json.loads(argument.value)]
while pending:
    value = pending.pop()
    if isinstance(value, list):
        pending.extend(value)
    elif isinstance(value, dict):
        payload = value.get("StrLit", {}).get("value")
        if payload in counts:
            counts[payload] += 1
        pending.extend(value.values())
for payload, count in counts.items():
    assert count == 1, f"{payload} has {count} emitted copies; the measured shared-body count is 1"
PY
