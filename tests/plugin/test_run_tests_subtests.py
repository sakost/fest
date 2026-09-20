"""Regression tests for subtest-failure detection in _run_tests.

unittest.subTest failures are emitted via the pytest_runtest_logreport hook
and are absent from runtestprotocol's return value; the parent call report
stays "passed" when only subtests fail. _run_tests must listen on the hook
or corpus-style suites (e.g. tomli's burntsushi tests) can never kill a
mutant.
"""

from __future__ import annotations

import textwrap

import pytest

from _fest_plugin import _run_tests


class _Driver:
    """Collect items, then run them through _run_tests inside runtestloop."""

    def __init__(self) -> None:
        self.result: str | None = None

    def pytest_runtestloop(self, session) -> bool:
        item_index = {item.nodeid: item for item in session.items}
        self.result = _run_tests(session, list(item_index), item_index)
        return True


def _run_in_nested_pytest(tmp_path, test_body: str) -> str:
    (tmp_path / "test_generated.py").write_text(textwrap.dedent(test_body))
    driver = _Driver()
    pytest.main(
        ["-p", "no:xdist", "-o", "addopts=", "-q", str(tmp_path)],
        plugins=[driver],
    )
    assert driver.result is not None, "driver did not run"
    return driver.result


def test_run_tests_detects_unittest_subtest_failure(tmp_path):
    result = _run_in_nested_pytest(
        tmp_path,
        """
        import unittest

        class TestSub(unittest.TestCase):
            def test_with_subtests(self):
                for i in range(3):
                    with self.subTest(i=i):
                        assert i != 1
        """,
    )
    assert result == "killed", "subtest-only failure must count as killed"


def test_run_tests_passing_subtests_survive(tmp_path):
    result = _run_in_nested_pytest(
        tmp_path,
        """
        import unittest

        class TestSub(unittest.TestCase):
            def test_with_subtests(self):
                for i in range(3):
                    with self.subTest(i=i):
                        assert i >= 0
        """,
    )
    assert result == "survived"
