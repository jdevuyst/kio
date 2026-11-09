import pathlib
import re
import sys

source = pathlib.Path(sys.argv[1]).read_text()


def body(name):
    marker = "-- Module fn `" + name + "`"
    assert source.count(marker) == 1, (name, "missing or ambiguous subject")
    tail = source.split(marker, 1)[1]
    return next(line for line in tail.splitlines() if " _pkg" in line and " = " in line)


for name in ["inline_calls", "inline_blocks", "inline_operators"]:
    code = body(name)
    assert " && " in code and " || " in code, (name, "native Boolean operators absent")
    assert "\\" not in code, (name, "inline branch lambda survived")

assert re.search(r"\bnot\s*\(", body("not")), "ordinary library NOT did not recover"
assert "if " in body("inline_effect"), "effectful RHS needs native branch selection"
assert "if " in body("opaque"), "opaque callback must remain conditional"
staged = body("staged")
assert staged.index("condition") < staged.index("factory") < staged.index("if "), "strict factory staging changed"
