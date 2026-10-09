from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Literal, TypeVar, TypedDict, Union

from rich_typed_host import (
    create_richTypedHost,
    KioHostType_api__Crate,
    KioViewHost_api__applyPoly_Arg0Source as PolySource,
    KioViewProj_api__Pack__openPack_Result_Stage1Source as OpenSource,
    kio_lift,
    kio_lift_stage,
)

if TYPE_CHECKING:
    from rich_typed_host import (
        KioApply1,
        KioFn,
        KioHostMk_api__Crate,
        KioViewHost_api__applyPoly_Arg0 as Poly,
        KioViewProj_api__Pack__openPack_Result_Stage1 as Open,
        KioNewtype_api__Box,
        KioNewtype_api__Shadow,
        KioNewtype_api__Functor,
        KioNewtypeMk_api__Box,
        KioViewCtor_api__Shadow__makeShadow_Arg0,
        RichTypedHostHost,
    )


T = TypeVar("T")
F = TypeVar("F")


stage_events: list[str] = []


class Identity(PolySource):
    def advance(self) -> KioFn[T, T]:  # pyright: ignore[reportInvalidTypeVarUse]
        stage_events.append("identity-stage")

        def identity(value: T) -> T:
            stage_events.append("identity-value")
            return value

        return kio_lift(identity)


class OpenString(OpenSource[str]):
    def advance(self) -> KioFn[T, str]:  # pyright: ignore[reportInvalidTypeVarUse]
        stage_events.append("opener-stage")

        def open_value(_value: T) -> str:
            stage_events.append("opener-value")
            return "opened"

        return kio_lift(open_value)


if TYPE_CHECKING:
    BoxString = KioNewtype_api__Box[str, str]
    BoxFunctor = KioNewtype_api__Functor[
        str, KioNewtypeMk_api__Box[str]
    ]
    ShadowBoxString = KioNewtype_api__Shadow[str, BoxString]


class ApiHost:
    def applyPoly(self, f: Poly) -> str:
        selected: KioFn[str, str] = f.advance()
        return selected("rank-N")

    def roundShadow(
        self, value: ShadowBoxString
    ) -> ShadowBoxString:
        return value

    def roundHkt(self, value: KioApply1[F, str]) -> KioApply1[F, str]:
        return value

    def roundFunctor(self, value: BoxFunctor) -> BoxFunctor:
        return value


class Host:
    def __init__(self) -> None:
        self.api = ApiHost()

    def KioHostIn_api_String(self, value: str) -> str:
        return value

    def KioHostOut_api_String(self, value: str) -> str:
        return value


host: RichTypedHostHost[str] = Host()
pkg = create_richTypedHost(host)

assert pkg.api.poly() == "rank-N"

boxed = pkg.api.KioType_Box.makeBox("hkt")
hkt_boxed = pkg.api.hkt(boxed)
assert pkg.api.KioType_Box.readBox(hkt_boxed) == "hkt"
box_functor = pkg.api.functor(pkg.api.boxFunctor(boxed))
applied_box: KioApply1[
    KioNewtypeMk_api__Box[str], str
] = pkg.api.KioType_Functor.readFunctor(box_functor)
assert pkg.api.KioType_Box.readBox(applied_box) == "hkt"
reconstructed_functor: BoxFunctor = pkg.api.KioType_Functor.makeFunctor(applied_box)
assert pkg.api.KioType_Box.readBox(
    pkg.api.KioType_Functor.readFunctor(pkg.api.functor(reconstructed_functor))
) == "hkt"
shadow_input: KioViewCtor_api__Shadow__makeShadow_Arg0[
    BoxString
] = {"left": boxed, "right": kio_lift_stage(Identity())}
shadow = pkg.api.KioType_Shadow.makeShadow(shadow_input)
shadow_payload = pkg.api.KioType_Shadow.readShadow(pkg.api.shadow(shadow))
assert pkg.api.KioType_Box.readBox(shadow_payload["left"]) == "hkt"
assert stage_events == []
selected_int: KioFn[int, int] = shadow_payload["right"].advance()
assert stage_events == ["identity-stage"]
assert selected_int(37) == 37
selected_text: KioFn[str, str] = shadow_payload["right"].advance()
assert selected_text("selected") == "selected"
assert stage_events == [
    "identity-stage", "identity-value", "identity-stage", "identity-value"
]

packed = pkg.api.pack({"witness": "existential"})
open_string: KioFn[Open[str], str] = pkg.api.KioType_Pack.openPack(packed).advance()
hidden = open_string(kio_lift_stage(OpenString()))
assert hidden == "opened"
assert stage_events[-2:] == ["opener-stage", "opener-value"]

recursive_base = pkg.api.KioType_Recursive.makeRecursive({"tag": "left", "value": None})
recursive = pkg.api.KioType_Recursive.makeRecursive({"tag": "right", "value": recursive_base})
recursive_view = pkg.api.KioType_Recursive.readRecursive(recursive)
assert recursive_view["tag"] == "right"
assert pkg.api.KioType_Recursive.readRecursive(recursive_view["value"]) == {"tag": "left", "value": None}

deep_recursive = recursive_base
for _ in range(1200):
    deep_recursive = pkg.api.KioType_Recursive.makeRecursive(
        {"tag": "right", "value": deep_recursive}
    )
for _ in range(1200):
    deep_view = pkg.api.KioType_Recursive.readRecursive(deep_recursive)
    assert deep_view["tag"] == "right"
    deep_recursive = deep_view["value"]
assert pkg.api.KioType_Recursive.readRecursive(deep_recursive) == {
    "tag": "left", "value": None
}

stage_count = len(stage_events)
assert selected_int(41) == 41
assert stage_events[stage_count:] == ["identity-value"]
for index in range(4000):
    event_count = len(stage_events)
    selected_again: KioFn[int, int] = shadow_payload["right"].advance()
    assert stage_events[event_count:] == ["identity-stage"]
    assert selected_again(index) == index
    assert stage_events[event_count:] == ["identity-stage", "identity-value"]


class Tail(TypedDict):
    left: str
    right: str

class Triple(TypedDict):
    left: str
    right: Tail


class Arm0(TypedDict):
    tag: Literal["left"]
    value: str


class Arm1(TypedDict):
    tag: Literal["right"]
    value: str


class Arm2(TypedDict):
    tag: Literal["right"]
    value: Union[Arm0, Arm1]


SumPayload = Union[Arm0, Arm2]


@dataclass(frozen=True)
class ProductStorage:
    value: Triple


@dataclass(frozen=True)
class SumStorage:
    value: SumPayload


events: list[str] = []
payload: Triple = {"left": "head", "right": {"left": "middle", "right": "tail"}}


def pack_product(value: Triple) -> KioApply1[KioHostMk_api__Crate, Triple]:
    assert value == payload
    events.append("pack-product")
    return KioHostType_api__Crate[Triple].from_native(ProductStorage(value))


def read_product(value: KioApply1[KioHostMk_api__Crate, Triple]) -> Triple:
    stored = KioHostType_api__Crate[Triple].to_native(value)
    assert isinstance(stored, ProductStorage)
    assert stored.value == payload
    events.append("read-product")
    return stored.value


product_result: Triple = pkg.api.concreteProductRoundtrip(kio_lift(pack_product), kio_lift(read_product), payload)
assert product_result == payload
assert events == ["pack-product", "read-product"]

sum_payloads: tuple[SumPayload, ...] = (
    {"tag": "left", "value": "first"},
    {"tag": "right", "value": {"tag": "left", "value": "middle"}},
    {"tag": "right", "value": {"tag": "right", "value": "last"}},
)
for index, sum_payload in enumerate(sum_payloads):
    def pack_sum(value: SumPayload) -> KioApply1[KioHostMk_api__Crate, SumPayload]:
        assert value == sum_payload
        events.append(f"pack-sum-{index}")
        return KioHostType_api__Crate[SumPayload].from_native(SumStorage(value))

    def read_sum(value: KioApply1[KioHostMk_api__Crate, SumPayload]) -> SumPayload:
        stored = KioHostType_api__Crate[SumPayload].to_native(value)
        assert isinstance(stored, SumStorage)
        assert stored.value == sum_payload
        events.append(f"read-sum-{index}")
        return stored.value

    sum_result: SumPayload = pkg.api.concreteSumRoundtrip(kio_lift(pack_sum), kio_lift(read_sum), sum_payload)
    assert sum_result == sum_payload

assert events == [
    "pack-product", "read-product",
    "pack-sum-0", "read-sum-0",
    "pack-sum-1", "read-sum-1",
    "pack-sum-2", "read-sum-2",
]

print("rich typed host ok")
