from rich_typed_host import KioViewHost_api__applyPoly_Arg0 as Poly, KioFn


def wrong(poly: Poly) -> int:
    integers: KioFn[int, int] = poly.advance()
    return integers("wrong")
