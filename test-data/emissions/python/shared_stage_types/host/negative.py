import stages_first as first
import stages_second as second


def selected(poly: first.KioViewExport_api__poly_Result) -> int:
    integers: second.KioFn[int, int] = poly.advance()
    return integers("wrong")


def nested(poly: first.KioViewExport_api__nested_Result) -> int:
    outer: second.KioViewExport_api__nested_Result_Stage1[int, int] = poly.advance()
    inner: first.KioFn[int, second.KioFn[str, int]] = outer.advance()
    return inner(7)(False)


def swapped(value: first.KioViewExport_api__captured_Result[int, str]) -> first.KioViewExport_api__captured_Result[str, int]:
    return value


def foreign(value: first.KioNewtype_api__Token) -> second.KioNewtype_api__Token:
    return value


def wrong_application(value: second.KioViewExport_api__applied_Result[first.KioHostMk_api__Box]) -> second.KioViewExport_api__applied_Result[second.KioHostMk_api__Box]:
    return value


def regeneralize(value: first.KioFn[int, int]) -> second.KioFn[str, str]:
    return value


def identity(value: int) -> int:
    return value


def lifted() -> int:
    integers: first.KioFn[int, int] = second.kio_lift(identity)
    selected: second.KioFn[int, int] = integers
    return selected("wrong")
