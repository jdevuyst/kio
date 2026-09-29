from __future__ import annotations

from typing import TYPE_CHECKING, Protocol, TypeVar

from rich_typed_host import create_richTypedHost

if TYPE_CHECKING:
    from rich_typed_host import (
        KioApply1,
        KioNewtype_V1_M1_C3_apiN3_Box,
        KioNewtype_V1_M1_C3_apiN6_Shadow,
        KioNewtype_V1_M1_C3_apiN7_Functor,
        KioNewtypeMk_V1_M1_C3_apiN3_Box,
        KioShape_V1_M1_C3_apiR4_CtorN6_ShadowN10_makeShadow_H1_P0,
        RichTypedHostHost,
    )


T = TypeVar("T")
F = TypeVar("F")


class Poly(Protocol):
    def __call__(self, value: T, /) -> T: ...


if TYPE_CHECKING:
    BoxString = KioNewtype_V1_M1_C3_apiN3_Box[str, str]
    BoxFunctor = KioNewtype_V1_M1_C3_apiN7_Functor[
        str, KioNewtypeMk_V1_M1_C3_apiN3_Box[str]
    ]
    ShadowBoxString = KioNewtype_V1_M1_C3_apiN6_Shadow[str, BoxString]


class ApiHost:
    def applyPoly(self, f: Poly) -> str:
        return f("rank-N")

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
    KioNewtypeMk_V1_M1_C3_apiN3_Box[str], str
] = pkg.api.KioType_Functor.readFunctor(box_functor)
assert applied_box == boxed
reconstructed_functor: BoxFunctor = pkg.api.KioType_Functor.makeFunctor(applied_box)
assert pkg.api.KioType_Functor.readFunctor(pkg.api.functor(reconstructed_functor)) == boxed
shadow_input: KioShape_V1_M1_C3_apiR4_CtorN6_ShadowN10_makeShadow_H1_P0[
    BoxString
] = {"_0": boxed, "_1": lambda value: value}
shadow = pkg.api.KioType_Shadow.makeShadow(shadow_input)
shadow_payload = pkg.api.KioType_Shadow.readShadow(pkg.api.shadow(shadow))
assert pkg.api.KioType_Box.readBox(shadow_payload["_0"]) == "hkt"
assert shadow_payload["_1"](37) == 37

packed = pkg.api.pack({"witness": "existential"})
hidden = pkg.api.KioType_Pack.openPack(packed)(lambda _value: "opened")
assert hidden == "opened"

recursive_base = pkg.api.KioType_Recursive.makeRecursive({"_0": None})
recursive = pkg.api.KioType_Recursive.makeRecursive({"Recursive": recursive_base})
recursive_view = pkg.api.KioType_Recursive.readRecursive(recursive)
assert "Recursive" in recursive_view

print("rich typed host ok")
