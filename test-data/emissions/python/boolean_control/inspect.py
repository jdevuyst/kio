import ast
import json
import pathlib
import sys

packages = []
for file in pathlib.Path(sys.argv[1]).rglob("*.py"):
    for node in ast.parse(file.read_text()).body:
        if isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id == "_KIO_IR" for target in node.targets):
            packages.append(json.loads(ast.literal_eval(node.value.args[0])))
assert len(packages) == 1, "expected one emitted runtime program"
module = next(module["module"] for module in packages[0]["modules"] if module["key"] == "main")
bodies = {item["FnDef"]["name"]: item["FnDef"]["body"] for item in module["items"] if "FnDef" in item}


def nodes(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from nodes(child)
    elif isinstance(value, list):
        for child in value:
            yield from nodes(child)


for name in ["inline_calls", "inline_blocks", "inline_operators", "inline_effect"]:
    entries = list(nodes(bodies[name]))
    assert not any("FnExpr" in entry for entry in entries), (name, "inline branch function survived")
    assert not any("LowClosureCall" in entry or "LowIndirectCall" in entry for entry in entries), (name, "inline branch invocation survived")
    expected = 1 if name == "inline_effect" else 2
    assert sum("EnrichedConditional" in entry for entry in entries) == expected, (name, "direct conditional children absent")

assert sum("LowClosureCall" in entry for entry in nodes(bodies["opaque"])) == 1, "opaque RHS invocation disappeared"
order = []
for entry in nodes(bodies["staged"]):
    if "LowHostCall" in entry:
        name = entry["LowHostCall"]["name"]
        if name in ["condition", "factory"]:
            order.append(name)
    if "EnrichedConditional" in entry:
        order.append("dispatch")
assert order == ["condition", "factory", "dispatch"], order
