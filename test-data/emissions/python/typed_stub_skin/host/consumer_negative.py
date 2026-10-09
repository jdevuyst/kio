# Causal contrast for consumer.py: this strict consumer deliberately omits
# the Right arm. Against the emitted three-arm TypedDict union, the final
# assert_never call must fail with the remaining arm still inhabited.
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
    labels = pkg.api.KioModule_labels
    choice = pkg.api.KioModule_main.echoChoice({
        "tag": "right", "value": {"tag": "right", "value": labels.KioType_Right.mk(29)},
    })

    if choice["tag"] == "left":
        left: int = labels.KioType_Left.get(choice["value"])
        _ = left
        return
    inner = choice["value"]
    if inner["tag"] == "left":
        middle: str = labels.KioType_Middle.get(inner["value"])
        _ = middle
        return
    remaining_right: int = labels.KioType_Right.get(inner["value"])
    _ = remaining_right
    assert_never(inner)


check(Host())
