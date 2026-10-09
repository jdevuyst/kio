from __future__ import annotations

from typing import TYPE_CHECKING, TypedDict
import stages_first as first
import stages_second as second

if TYPE_CHECKING:
    from stages_first import KioFn, KioApply1, KioHostMk_api__Box as BoxMarker


class Host:
    def __init__(self) -> None:
        self.api = self
        self.calls = 0

    def mark(self) -> None:
        self.calls += 1


left_host = Host()
right_host = Host()
left = first.create_stagesFirst(left_host)
right = second.create_stagesSecond(right_host)

forward: second.KioViewExport_api__poly_Result = left.api.poly()
reverse: first.KioViewExport_api__poly_Result = right.api.poly()
assert left_host.calls == right_host.calls == 0
integers: second.KioFn[int, int] = forward.advance()
texts: first.KioFn[str, str] = reverse.advance()
assert left_host.calls == right_host.calls == 1
assert integers(37) == 37
assert texts("selected") == "selected"

nested: second.KioViewExport_api__nested_Result = left.api.nested()
outer: first.KioViewExport_api__nested_Result_Stage1[int, int] = nested.advance()
assert left_host.calls == 2
inner: second.KioFn[int, first.KioFn[str, int]] = outer.advance()
assert left_host.calls == 3
assert inner(41)("unused") == 41

captured = left.api.captured(17, "retained")


class UnitInt(TypedDict):
    left: None
    right: int


captured_call: KioFn[UnitInt, str] = captured.advance()
assert captured_call({"left": None, "right": 17}) == "retained"

application: second.KioViewExport_api__applied_Result[BoxMarker] = right.api.applied()
applied: KioFn[KioApply1[BoxMarker, int], KioApply1[BoxMarker, int]] = application.advance()
boxed = first.KioHostType_api__Box[int].from_native(73)
roundtrip = applied(left.api.boxId(boxed))
assert first.KioHostType_api__Box[int].to_native(roundtrip) == 73
assert left_host.calls == 3
assert right_host.calls == 1
print("shared stages ok")
