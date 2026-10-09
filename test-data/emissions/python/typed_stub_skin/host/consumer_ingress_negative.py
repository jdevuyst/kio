# Causal input-precision contrast for consumer.py. Every malformed dict below
# would be accepted by a broad object parameter; the emitted Pair / Choice
# parameter types must reject each call independently under strict Pyright.
import sys

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


def check(host: BuildPythonTypedStubSkinHost[int, str]) -> None:
    pkg = create_buildPythonTypedStubSkin(host)
    labels = pkg.api.KioModule_labels

    pkg.api.KioModule_main.echoPair({"left": labels.KioType_Name.mk("missing Count")})
    pkg.api.KioModule_main.echoPair({"left": labels.KioType_Name.mk("wrong Count"), "right": "seven"})
    pkg.api.KioModule_main.echoChoice({"tag": "unknown", "value": labels.KioType_Left.mk(1)})
    pkg.api.KioModule_main.echoChoice({"tag": "right", "value": {"tag": "left", "value": 2}})


check(Host())
