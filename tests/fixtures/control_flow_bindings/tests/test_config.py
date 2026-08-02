from src.config import BACKOFF, MAX_RETRIES


def test_max_retries_is_positive():
    assert MAX_RETRIES > 0


def test_backoff_is_positive():
    assert BACKOFF > 0


def test_max_retries_at_least_three():
    assert MAX_RETRIES >= 3
