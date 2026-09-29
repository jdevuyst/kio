# A Python host reading the emitted typed stub at its true runtime shape.
# `create_...` returns the branded handle. `Pair` crosses as the exact
# TypedDict `{ "Name": str, "Count": int }`; the three-arm `Choice`
# crosses as the discriminated union `{ "Left": int } |
# { "Middle": str } | { "Right": int }`. The host constructs every
# shape, reads every product field, and narrows every sum arm using
# ordinary keyed-dict operations. If the `.pyi` loses that precision,
# strict Pyright rejects this independently-authored consumer.
import sys
from typing import NoReturn, TypedDict

from build_python_typed_stub_skin import (
    BuildPythonTypedStubSkinHost,
    create_buildPythonTypedStubSkin,
)


class ApiCapabilities:
    def print(self, p0: str) -> None:
        sys.stdout.write(p0)


class Host:
    def __init__(self) -> None:
        self.api = ApiCapabilities()

    def KioHostIn_api_I32(self, value: int) -> int:
        return value

    def KioHostOut_api_I32(self, value: int) -> int:
        return value

    def KioHostIn_api_String(self, value: str) -> str:
        return value

    def KioHostOut_api_String(self, value: str) -> str:
        return value


class LeftChoice(TypedDict):
    Left: int


class MiddleChoice(TypedDict):
    Middle: str


class RightChoice(TypedDict):
    Right: int


def assert_never(value: NoReturn) -> NoReturn:
    raise AssertionError(f"unreachable value: {value!r}")


def check(host: BuildPythonTypedStubSkinHost[int, str]) -> None:
    pkg = create_buildPythonTypedStubSkin(host)

    # A host-backed export: the package calls back into `api.print`.
    pkg.api.KioModule_main.greet("hello from kio\n")

    # Product values are precisely typed in both directions.
    pair = pkg.api.KioModule_main.make("kio", 3)
    name: str = pair["Name"]
    count: int = pair["Count"]
    echoed_pair = pkg.api.KioModule_main.echoPair({"Name": "host", "Count": 7})
    echoed_name: str = echoed_pair["Name"]
    echoed_count: int = echoed_pair["Count"]

    # Each one-key Choice arm can be constructed as an ordinary dict.
    # Required-key presence narrows the emitted TypedDict union, and the
    # final NoReturn assertion proves that all three arms were handled.
    left_choice: LeftChoice = {"Left": 11}
    middle_choice: MiddleChoice = {"Middle": "middle"}
    right_choice: RightChoice = {"Right": 29}
    choices = (
        pkg.api.KioModule_main.echoChoice(left_choice),
        pkg.api.KioModule_main.echoChoice(middle_choice),
        pkg.api.KioModule_main.echoChoice(right_choice),
    )
    rendered: list[str] = []
    for choice in choices:
        if "Left" in choice:
            left: int = choice["Left"]
            rendered.append(f"left:{left}")
            continue
        if "Middle" in choice:
            middle: str = choice["Middle"]
            rendered.append(f"middle:{middle}")
            continue
        if "Right" in choice:
            right: int = choice["Right"]
            rendered.append(f"right:{right}")
            continue
        assert_never(choice)

    # A labels-generated newtype namespace: constructor / projector. The
    # internal rep is opaque (`object`); the payload is its role type.
    wrapped = pkg.api.KioModule_labels.KioType_Name.mk(name)
    unwrapped: str = pkg.api.KioModule_labels.KioType_Name.get(wrapped)

    # A Python-keyword export is reached with `getattr`, typed `object`.
    reserved = getattr(pkg.api.KioModule_main, "return")

    _ = (count, echoed_name, echoed_count, rendered, unwrapped, reserved)


check(Host())
