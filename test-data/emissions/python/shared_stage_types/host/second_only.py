import stages_second as second


def selected(poly: second.KioViewExport_api__poly_Result) -> str:
    texts: second.KioFn[str, str] = poly.advance()
    return texts("selected")


def applied(value: second.KioViewExport_api__applied_Result[second.KioHostMk_api__Box]) -> second.KioViewExport_api__applied_Result[second.KioHostMk_api__Box]:
    return value
