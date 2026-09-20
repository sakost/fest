from src.calc import add
from src.consumer import at_max, double_add


def test_add():
    assert add(1, 2) == 3


def test_double_add():
    assert double_add(1, 2) == 6


def test_at_max_true():
    assert at_max(100) is True


def test_at_max_false():
    assert at_max(50) is False
