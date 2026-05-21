from src.factory import make_counter


def test_counter_returns_one():
    counter = make_counter()
    assert counter() == 1
