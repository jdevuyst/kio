from __future__ import annotations

from typing import TYPE_CHECKING, Generic, Literal, TypeAlias, TypeVar, TypedDict, cast

from opaque_substitution import (
    create_opaqueSubstitution,
    KioApply1,
    KioHostMk_api__Box,
    KioHostType_api__Box,
)

if TYPE_CHECKING:
    from opaque_substitution import (
        OpaqueSubstitutionHost,
        KioShape_M1_C3_apiR6_ExportN7_putLeft_U4_left as PutLeftLeft,
        KioShape_M1_C3_apiR6_ExportN7_putLeft_U4_right as PutLeftRight,
        KioShape_M1_C3_apiR6_ExportN8_putRight_U4_left as PutRightLeft,
        KioShape_M1_C3_apiR6_ExportN8_putRight_U4_right as PutRightRight,
    )

T = TypeVar("T")
A = TypeVar("A")
B = TypeVar("B")


class Pair(TypedDict):
    left: int
    right: int


if TYPE_CHECKING:
    class Left(TypedDict, Generic[A]):
        tag: Literal["left"]
        value: A

    class Right(TypedDict, Generic[B]):
        tag: Literal["right"]
        value: B

    Sum: TypeAlias = Left[A] | Right[B]


class ApiHost:
    def __init__(self) -> None:
        self.unbox_calls = 0
        self.consumed: list[tuple[int, int]] = []
        self.packed = 0
        self.length_calls = 0

    def unbox(self, boxed: KioApply1[KioHostMk_api__Box, T]) -> T:
        self.unbox_calls += 1
        return cast(T, KioHostType_api__Box[T].to_native(boxed))

    def consume(self, head: int, tail: int) -> int:
        self.consumed.append((head, tail))
        return head

    def pack(self, value: T) -> KioApply1[KioHostMk_api__Box, T]:
        self.packed += 1
        return KioHostType_api__Box[T].from_native(value)

    def textLength(self, value: str) -> int:
        self.length_calls += 1
        return len(value)


class Host:
    def __init__(self) -> None:
        self.api = ApiHost()

    def KioHostIn_api_Number(self, value: int) -> int:
        return value

    def KioHostOut_api_Number(self, value: int) -> int:
        return value

    def KioHostIn_api_Text(self, value: str) -> str:
        return value

    def KioHostOut_api_Text(self, value: str) -> str:
        return value


host = Host()
typed_host: OpaqueSubstitutionHost[int, str] = host
pkg = create_opaqueSubstitution(typed_host)
direct: Pair = {"left": 173, "right": 409}
relayed: Pair = {"left": 291, "right": 617}
direct_box = KioHostType_api__Box[Pair].from_native(direct)
relayed_box = KioHostType_api__Box[Pair].from_native(relayed)
assert pkg.api.direct(direct_box) == 173
assert pkg.api.throughRelay(relayed_box) == 291
assert host.api.unbox_calls == 2
assert host.api.consumed == [(173, 409), (291, 617)]

# Select the absent index through the exact producer type, then check the
# independently authored equivalent sum under invariant Box without conversion.
left_selected: KioApply1[KioHostMk_api__Box, PutLeftLeft[int] | PutLeftRight[str]] = pkg.api.putLeft(73)
right_selected: KioApply1[KioHostMk_api__Box, PutRightLeft[int] | PutRightRight[str]] = pkg.api.putRight("right")
left: KioApply1[KioHostMk_api__Box, Sum[int, str]] = left_selected
right: KioApply1[KioHostMk_api__Box, Sum[int, str]] = right_selected
assert host.api.packed == 2
assert pkg.api.readSum(left) == 73
assert pkg.api.readSum(right) == 5
assert host.api.unbox_calls == 4
assert host.api.length_calls == 1
assert host.api.consumed == [(173, 409), (291, 617)]
print("opaque structural substitution ok")
