from __future__ import annotations

from typing import TYPE_CHECKING

from build_python_typed_stub_skin import create_buildPythonTypedStubSkin

if TYPE_CHECKING:
    from build_python_typed_stub_skin import BuildPythonTypedStubSkinHost


class ApiCapabilities:
    def __init__(self) -> None:
        self.messages: list[str] = []

    def print(self, p0: str) -> None:
        self.messages.append(p0)


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


def check(host: BuildPythonTypedStubSkinHost[int, str], observed: Host) -> None:
    pkg = create_buildPythonTypedStubSkin(host)
    pkg.api.KioModule_main.greet("hello from kio\n")
    pair = pkg.api.KioModule_main.make("kio", 3)
    name: str = pair["Name"]
    count: int = pair["Count"]
    wrapped = pkg.api.KioModule_labels.KioType_Name.mk(name)
    unwrapped: str = pkg.api.KioModule_labels.KioType_Name.get(wrapped)
    reserved: object = getattr(pkg.api.KioModule_main, "return")

    assert count == 3
    assert unwrapped == "kio"
    assert callable(reserved)
    assert observed.api.messages == ["hello from kio\n"]


runtime_host = Host()
check(runtime_host, runtime_host)
