"""Tests for fest thread cleanup registry and fixture."""

from __future__ import annotations


def test_cleanup_registry_runs_callbacks_in_lifo_order():
    import _fest_plugin
    cleanup = _fest_plugin._ThreadCleanupRegistry()
    calls: list[str] = []

    cleanup.register(calls.append, "first")
    cleanup.register(calls.append, "second")

    cleanup.run_all()

    assert calls == ["second", "first"]


def test_cleanup_registry_collects_errors():
    import _fest_plugin
    cleanup = _fest_plugin._ThreadCleanupRegistry()

    def boom():
        raise ValueError("nope")

    cleanup.register(boom)
    cleanup.register(lambda: None)

    errors = cleanup.run_all()
    assert len(errors) == 1
    assert isinstance(errors[0], ValueError)


def test_cleanup_registry_clears_after_run():
    import _fest_plugin
    cleanup = _fest_plugin._ThreadCleanupRegistry()
    calls: list[int] = []
    cleanup.register(calls.append, 1)
    cleanup.run_all()
    cleanup.run_all()
    assert calls == [1]
