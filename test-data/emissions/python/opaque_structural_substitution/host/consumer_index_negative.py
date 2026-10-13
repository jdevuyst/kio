from typing import TYPE_CHECKING

from consumer import Sum

if TYPE_CHECKING:
    from opaque_substitution import (
        KioApply1,
        KioHostMk_api__Box,
        OpaqueSubstitution,
        KioShape_M1_C3_apiR6_ExportN8_putRight_U4_left as Left,
        KioShape_M1_C3_apiR6_ExportN8_putRight_U4_right as Right,
    )


def wrong(pkg: OpaqueSubstitution[int, str]) -> KioApply1[KioHostMk_api__Box, Sum[int, str]]:
    selected: KioApply1[KioHostMk_api__Box, Left[int] | Right[bool]] = pkg.api.putRight(True)
    return selected
