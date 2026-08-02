from src.nested_classes import Outer


def test_inner_value():
    assert Outer.Inner.VALUE == 42


def test_inner_compute_doubles():
    assert Outer.Inner().compute() == 84
