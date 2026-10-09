# A Python host reading the emitted typed stub at its true runtime shape.
# `create_...` returns the branded handle. `Pair` crosses as the exact
# TypedDict `{ "left": Name, "right": Count }`; the three-arm `Choice`
# crosses as nested discriminated `tag`/`value` unions carrying sealed
# Left, Middle, and Right values. The host constructs every shape through
# the public newtype methods and narrows every sum arm. If the `.pyi` loses that precision,
# strict Pyright rejects this independently-authored consumer.
import sys
from typing import NoReturn

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


def assert_never(value: NoReturn) -> NoReturn:
    raise AssertionError(f"unreachable value: {value!r}")


def check(host: BuildPythonTypedStubSkinHost[int, str]) -> None:
    pkg = create_buildPythonTypedStubSkin(host)

    # A host-backed export: the package calls back into `api.print`.
    pkg.api.KioModule_main.greet("hello from kio\n")
    labels = pkg.api.KioModule_labels

    # Product values are precisely typed in both directions.
    pair = pkg.api.KioModule_main.make("kio", 3)
    name: str = labels.KioType_Name.get(pair["left"])
    count: int = labels.KioType_Count.get(pair["right"])
    echoed_pair = pkg.api.KioModule_main.echoPair({
        "left": labels.KioType_Name.mk("host"),
        "right": labels.KioType_Count.mk(7),
    })
    echoed_name: str = labels.KioType_Name.get(echoed_pair["left"])
    echoed_count: int = labels.KioType_Count.get(echoed_pair["right"])

    # Each Choice arm is built with exact nested tagged dictionaries.
    # Literal tags narrow the emitted TypedDict union, and the
    # final NoReturn assertion proves that all three arms were handled.
    choices = (
        pkg.api.KioModule_main.echoChoice({
            "tag": "left", "value": labels.KioType_Left.mk(11),
        }),
        pkg.api.KioModule_main.echoChoice({
            "tag": "right", "value": {
                "tag": "left", "value": labels.KioType_Middle.mk("middle"),
            },
        }),
        pkg.api.KioModule_main.echoChoice({
            "tag": "right", "value": {
                "tag": "right", "value": labels.KioType_Right.mk(29),
            },
        }),
    )
    rendered: list[str] = []
    for choice in choices:
        if choice["tag"] == "left":
            left: int = labels.KioType_Left.get(choice["value"])
            rendered.append(f"left:{left}")
            continue
        inner = choice["value"]
        if inner["tag"] == "left":
            middle: str = labels.KioType_Middle.get(inner["value"])
            rendered.append(f"middle:{middle}")
            continue
        if inner["tag"] == "right":
            right: int = labels.KioType_Right.get(inner["value"])
            rendered.append(f"right:{right}")
            continue
        assert_never(inner)

    # A labels-generated newtype namespace: constructor / projector. The
    # internal rep is opaque (`object`); the payload is its role type.
    wrapped = labels.KioType_Name.mk(name)
    unwrapped: str = labels.KioType_Name.get(wrapped)

    # A Python-keyword export is reached with `getattr`, typed `object`.
    reserved = getattr(pkg.api.KioModule_main, "return")

    _ = (count, echoed_name, echoed_count, rendered, unwrapped, reserved)


check(Host())
