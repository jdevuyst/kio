from __future__ import annotations

from typing import TYPE_CHECKING, Generic, TypeVar, TypedDict

from tail_origin import create_tailOrigin
from tail_peer import create_tailPeer

if TYPE_CHECKING:
    from tail_origin import KioFn

T = TypeVar("T")


class Result(TypedDict, Generic[T]):
    left: T
    right: KioFn[None, T]


depth = 2048
events: list[tuple[int, int]] = []
stored = [0, 0]


class ApiHost:
    def __init__(self, owner: int) -> None:
        self.owner = owner

    def mark(self, code: int) -> None:
        events.append((self.owner, code))

    def keep(self, value: T) -> T:
        stored[self.owner] += 1
        return {"value": value}["value"]


class Host:
    def __init__(self, owner: int) -> None:
        self.api = ApiHost(owner)

    def KioHostIn_api_Number(self, value: int) -> int:
        return value

    def KioHostOut_api_Number(self, value: int) -> int:
        return value


origin = create_tailOrigin(Host(0)).api
peer = create_tailPeer(Host(1)).api


def check(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def trace(link: int, root: int) -> None:
    expected = [(i % 2, link) for i in range(depth - 1, -1, -1)] + [(0, root)]
    check(events == expected, "owner or stage order")
    events.clear()


values = [origin.valueRoot()]
for i in range(depth):
    linked = origin.valueLink(values[-1]) if i % 2 == 0 else peer.valueLink(values[-1])
    kept = peer.keepValue(linked) if i % 2 == 0 else origin.keepValue(linked)
    values.extend((linked, kept))
check(not events and stored == [depth // 2, depth // 2], "value transport effects")
for _ in range(2):
    result = values[-1](None)
    trace(11, 12)
    check(result["left"] == 37 and result["right"](None) == 41, "nested value result")
    check(events == [(0, 13)], "nested value callback owner")
    events.clear()

stages = [origin.typeRoot()]
for i in range(depth):
    linked_stage = origin.typeLink(stages[-1]) if i % 2 == 0 else peer.typeLink(stages[-1])
    kept_stage = peer.keepType(linked_stage) if i % 2 == 0 else origin.keepType(linked_stage)
    stages.extend((linked_stage, kept_stage))
check(not events and stored == [depth, depth], "type transport effects")

# Ordinary inference fixes each selected type without adding runtime type arguments.
number: KioFn[int, Result[int]] = stages[-1].advance()
trace(21, 22)
text: KioFn[str, Result[str]] = stages[-1].advance()
trace(21, 22)
number_result = number(91)
number_value: int = number_result["left"]
number_nested: KioFn[None, int] = number_result["right"]
check(number_value == 91 and events == [(0, 23)], "selected number result")
events.clear()
check(number_nested(None) == 91 and events == [(0, 24)], "nested number result")
events.clear()
again = number(93)
check(again["left"] == 93 and events == [(0, 23)], "reused selected result")
events.clear()
check(again["right"](None) == 93 and events == [(0, 24)], "reused nested result")
events.clear()
text_result = text("nested")
text_value: str = text_result["left"]
text_nested: KioFn[None, str] = text_result["right"]
check(text_value == "nested" and events == [(0, 23)], "selected text result")
events.clear()
check(text_nested(None) == "nested" and events == [(0, 24)], "nested text result")
events.clear()
check(stored == [depth, depth], "invocation-time storage")
print("independent tail callables ok")
