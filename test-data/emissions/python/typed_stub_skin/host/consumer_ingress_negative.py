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

    pkg.api.KioModule_main.echoPair({"Name": "missing Count"})
    pkg.api.KioModule_main.echoPair({"Name": "wrong Count", "Count": "seven"})
    pkg.api.KioModule_main.echoChoice({"Unknown": 1})
    pkg.api.KioModule_main.echoChoice({"Middle": 2})


check(Host())
