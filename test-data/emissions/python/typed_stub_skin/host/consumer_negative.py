# Causal contrast for consumer.py: this strict consumer deliberately omits
# the Right arm. Against the emitted three-arm TypedDict union, the final
# assert_never call must fail with the remaining arm still inhabited.
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


class RightChoice(TypedDict):
    Right: int


def assert_never(value: NoReturn) -> NoReturn:
    raise AssertionError(f"unreachable value: {value!r}")


def check(host: BuildPythonTypedStubSkinHost[int, str]) -> None:
    pkg = create_buildPythonTypedStubSkin(host)
    right_choice: RightChoice = {"Right": 29}
    choice = pkg.api.KioModule_main.echoChoice(right_choice)

    if "Left" in choice:
        left: int = choice["Left"]
        _ = left
        return
    if "Middle" in choice:
        middle: str = choice["Middle"]
        _ = middle
        return
    remaining_right: int = choice["Right"]
    _ = remaining_right
    assert_never(choice)


check(Host())
