import stages_first as first


def selected(poly: first.KioViewExport_api__poly_Result) -> int:
    integers: first.KioFn[int, int] = poly.advance()
    return integers(17)


def captured(value: first.KioViewExport_api__captured_Result[int, str]) -> first.KioViewExport_api__captured_Result[int, str]:
    return value
