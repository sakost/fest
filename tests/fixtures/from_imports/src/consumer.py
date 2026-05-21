"""Consumer using `from`-imports."""

from src.calc import MAX, add


def double_add(a: int, b: int) -> int:
    return add(a, b) * 2


def at_max(value: int) -> bool:
    return value == MAX
