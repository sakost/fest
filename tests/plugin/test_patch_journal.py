"""Tests for fest plugin's PatchJournal class."""

from __future__ import annotations

from _fest_plugin import PatchJournal


def test_rollback_restores_in_reverse_order():
    state = []
    journal = PatchJournal()
    journal.append(state.append, "first")
    journal.append(state.append, "second")
    journal.append(state.append, "third")

    errors = journal.rollback()

    assert state == ["third", "second", "first"]
    assert errors == []


def test_rollback_clears_entries():
    journal = PatchJournal()
    state = []
    journal.append(state.append, "x")
    journal.rollback()
    journal.rollback()

    assert state == ["x"]


def test_rollback_continues_after_undo_raises():
    state = []

    def boom():
        raise RuntimeError("undo failed")

    journal = PatchJournal()
    journal.append(state.append, "first")
    journal.append(boom)
    journal.append(state.append, "third")

    errors = journal.rollback()

    assert state == ["third", "first"]
    assert len(errors) == 1
    assert isinstance(errors[0], RuntimeError)
