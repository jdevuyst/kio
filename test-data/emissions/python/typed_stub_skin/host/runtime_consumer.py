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
    labels = pkg.api.KioModule_labels
    pair = pkg.api.KioModule_main.make("kio", 3)
    name: str = labels.KioType_Name.get(pair["left"])
    count: int = labels.KioType_Count.get(pair["right"])
    echoed = pkg.api.KioModule_main.echoPair({
        "left": labels.KioType_Name.mk("host"),
        "right": labels.KioType_Count.mk(7),
    })
    wrapped = labels.KioType_Name.mk(name)
    unwrapped: str = labels.KioType_Name.get(wrapped)
    choices = (
        pkg.api.KioModule_main.echoChoice({"tag": "left", "value": labels.KioType_Left.mk(11)}),
        pkg.api.KioModule_main.echoChoice({"tag": "right", "value": {
            "tag": "left", "value": labels.KioType_Middle.mk("middle"),
        }}),
        pkg.api.KioModule_main.echoChoice({"tag": "right", "value": {
            "tag": "right", "value": labels.KioType_Right.mk(29),
        }}),
    )
    reserved: object = getattr(pkg.api.KioModule_main, "return")

    assert count == 3
    assert labels.KioType_Name.get(echoed["left"]) == "host"
    assert labels.KioType_Count.get(echoed["right"]) == 7
    assert labels.KioType_Left.get(choices[0]["value"]) == 11
    assert labels.KioType_Middle.get(choices[1]["value"]["value"]) == "middle"
    assert labels.KioType_Right.get(choices[2]["value"]["value"]) == 29
    assert unwrapped == "kio"
    assert callable(reserved)
    assert observed.api.messages == ["hello from kio\n"]


runtime_host = Host()
check(runtime_host, runtime_host)
