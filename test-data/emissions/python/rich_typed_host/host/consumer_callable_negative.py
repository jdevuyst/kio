from consumer_callable import Box, CallbackHost, Shape, Token
from rich_typed_host import KioFn


def widen_shape(value: KioFn[Shape[Token], int]) -> KioFn[Shape[object], int]:
    return value


def narrow_shape(value: KioFn[Shape[object], int]) -> KioFn[Shape[Token], int]:
    return value


def widen_host(value: CallbackHost[Token]) -> CallbackHost[object]:
    return value


def narrow_host(value: CallbackHost[object]) -> CallbackHost[Token]:
    return value


def call_surface(value: KioFn[int, str]) -> None:
    value(value=1)
    value()
    value(1, 2)
    value("wrong")


def widen_box(value: KioFn[Box[Token], int]) -> KioFn[Box[object], int]:
    return value


def wrong_result(value: KioFn[int, Token]) -> KioFn[int, str]:
    return value
