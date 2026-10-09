from typing import Callable, NoReturn

from qualifier_hygiene import QualifierHygiene


def requires_bottom(callback: Callable[[], NoReturn]) -> None:
    _ = callback


def use(package: QualifierHygiene) -> None:
    requires_bottom(package.api.typing)
