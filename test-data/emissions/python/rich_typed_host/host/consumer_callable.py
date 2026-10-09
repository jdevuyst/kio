from __future__ import annotations

from typing import Callable, Generic, Protocol, TypeVar, TypedDict

from rich_typed_host import KioFn


T = TypeVar("T")


class Shape(TypedDict, Generic[T]):
    value: T


class Token:
    pass


class Box(Generic[T]):
    value: T


class CallbackHost(Protocol[T]):
    def call(self, callback: KioFn[Shape[T], int], /) -> int: ...


def from_function(value: Callable[[Shape[Token]], Token]) -> KioFn[Shape[Token], Token]:
    return value


def to_function(value: KioFn[Shape[Token], Token]) -> Callable[[Shape[Token]], Token]:
    return value


def invoke(value: KioFn[Shape[Token], Token], token: Token) -> Token:
    return value({"value": token})


class Native:
    def __call__(self, value: Shape[Token], /) -> Token:
        return value["value"]


native: KioFn[Shape[Token], Token] = Native()


def callable_variance(value: KioFn[object, Token]) -> KioFn[Token, object]:
    return value
