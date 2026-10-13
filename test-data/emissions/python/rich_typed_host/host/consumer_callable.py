from __future__ import annotations

from typing import Callable, Generic, Protocol, TypeVar, TypedDict

from rich_typed_host import KioFn, kio_lift


T = TypeVar("T")


class Shape(TypedDict, Generic[T]):
    value: T


class Token:
    pass


class Box(Generic[T]):
    value: T


# Mutable Shape is invariant; Pyright misinfers variance through the callback Protocol.
class CallbackHost(Protocol[T]):  # pyright: ignore[reportInvalidTypeVarUse]
    def call(self, callback: KioFn[Shape[T], int], /) -> int: ...


def from_function(value: Callable[[Shape[Token]], Token]) -> KioFn[Shape[Token], Token]:
    return kio_lift(value)


def to_function(value: KioFn[Shape[Token], Token]) -> Callable[[Shape[Token]], Token]:
    return value


def invoke(value: KioFn[Shape[Token], Token], token: Token) -> Token:
    return value({"value": token})


class Native:
    def __call__(self, value: Shape[Token], /) -> Token:
        return value["value"]


native: KioFn[Shape[Token], Token] = kio_lift(Native())


def callable_variance(value: KioFn[object, Token]) -> KioFn[Token, object]:
    return value
